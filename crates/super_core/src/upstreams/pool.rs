//! Connections kept for the next requests to the same upstream: idle HTTP/1.1 connections (one
//! request at a time), and open HTTP/2 connections (shared by up to `max_h2_streams` requests).
//! Taking one allocates nothing.

use std::{
    collections::{HashMap, VecDeque},
    os::fd::RawFd,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, PoisonError,
    },
    time::Instant,
};

use hyper::client::conn::{http1, http2};

use super::{
    body::UpstreamBody,
    connector::{HttpSession, Sender},
};

/// Shards of the pool, so requests to different upstreams don't wait for each other
const SHARDS: usize = 16;

/// Idle connections kept per upstream, at most
const MAX_IDLE: usize = 128;

struct Idle {
    sender: http1::SendRequest<UpstreamBody>,
    fd: RawFd,
    expires: Option<Instant>,
}

struct Shared {
    sender: http2::SendRequest<UpstreamBody>,
    fd: RawFd,
    streams: Arc<AtomicUsize>,
}

#[derive(Default)]
struct Upstream {
    idle: VecDeque<Idle>,
    shared: Vec<Shared>,
}

/// A request on an HTTP/2 connection, counted until dropped
pub struct H2Stream(Arc<AtomicUsize>);

impl Drop for H2Stream {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct Pool {
    shards: [Mutex<HashMap<u64, Upstream>>; SHARDS],
}

impl Pool {
    pub fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| Mutex::default()),
        }
    }

    fn shard(&self, key: u64) -> std::sync::MutexGuard<'_, HashMap<u64, Upstream>> {
        let index = usize::try_from(key % SHARDS as u64).unwrap_or_default();
        self.shards
            .get(index)
            .unwrap_or(&self.shards[0])
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// A connection kept for `key`: an HTTP/2 one with room for another request, else the
    /// connection idle the shortest time
    pub fn checkout(&self, key: u64, max_h2_streams: usize) -> Option<HttpSession> {
        let mut shard = self.shard(key);
        let upstream = shard.get_mut(&key)?;
        upstream.shared.retain(|shared| !shared.sender.is_closed());
        let room = upstream
            .shared
            .iter()
            .find(|shared| shared.streams.load(Ordering::Relaxed) < max_h2_streams.max(1));
        if let Some(shared) = room {
            shared.streams.fetch_add(1, Ordering::Relaxed);
            let stream = H2Stream(shared.streams.clone());
            return Some(HttpSession {
                sender: Sender::H2(shared.sender.clone(), stream),
                key,
                fd: shared.fd,
            });
        }
        let now = Instant::now();
        while let Some(idle) = upstream.idle.pop_back() {
            let expired = idle.expires.is_some_and(|expires| expires <= now);
            if expired || idle.sender.is_closed() {
                continue;
            }
            return Some(HttpSession {
                sender: Sender::H1(idle.sender),
                key,
                fd: idle.fd,
            });
        }
        None
    }

    /// Keeps an idle HTTP/1.1 connection, until `expires`
    pub fn keep(
        &self,
        key: u64,
        sender: http1::SendRequest<UpstreamBody>,
        fd: RawFd,
        expires: Option<Instant>,
    ) {
        let mut shard = self.shard(key);
        let upstream = shard.entry(key).or_default();
        if upstream.idle.len() >= MAX_IDLE {
            // The oldest goes
            upstream.idle.pop_front();
        }
        upstream.idle.push_back(Idle {
            sender,
            fd,
            expires,
        });
    }

    /// Shares a new HTTP/2 connection, which carries a request already
    pub fn share(&self, key: u64, sender: http2::SendRequest<UpstreamBody>, fd: RawFd) -> H2Stream {
        let streams = Arc::new(AtomicUsize::new(1));
        let mut shard = self.shard(key);
        shard.entry(key).or_default().shared.push(Shared {
            sender,
            fd,
            streams: streams.clone(),
        });
        H2Stream(streams)
    }
}
