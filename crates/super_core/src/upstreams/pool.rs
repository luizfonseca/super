//! Connections kept for the next requests to the same upstream: idle HTTP/1.1 connections (one
//! request at a time), and open HTTP/2 connections (shared by up to `max_h2_streams` requests).
//! Taking one allocates nothing.
//!
//! Each thread keeps the connections it releases in a shard of its own, as nginx's workers do:
//! threads proxying to the same upstream don't wait for each other's lock, and a connection's
//! requests run on the thread its driver runs on (no thread wakes another for each request). A
//! thread without one opens its own.

use std::{
    cell::Cell,
    collections::VecDeque,
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
use crate::hash::PrehashedMap;

/// Shards of the pool: threads take one each, in turn
const SHARDS: usize = 64;

/// The shard of the next thread
static NEXT_SHARD: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// This thread's shard
    static SHARD: Cell<Option<usize>> = const { Cell::new(None) };
}

/// The shard of this thread
fn own_shard() -> usize {
    SHARD.with(|shard| {
        shard.get().unwrap_or_else(|| {
            let index = NEXT_SHARD.fetch_add(1, Ordering::Relaxed) % SHARDS;
            shard.set(Some(index));
            index
        })
    })
}

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

type Shard = PrehashedMap<Upstream>;

pub struct Pool {
    shards: [Mutex<Shard>; SHARDS],
}

impl Pool {
    pub fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| Mutex::default()),
        }
    }

    fn lock(&self, index: usize) -> std::sync::MutexGuard<'_, Shard> {
        self.shards
            .get(index % SHARDS)
            .unwrap_or(&self.shards[0])
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// A connection this thread kept for `key`
    pub fn checkout(&self, key: u64, max_h2_streams: usize) -> Option<HttpSession> {
        take(&mut self.lock(own_shard()), key, max_h2_streams)
    }

    /// Keeps an idle HTTP/1.1 connection, until `expires`
    pub fn keep(
        &self,
        key: u64,
        sender: http1::SendRequest<UpstreamBody>,
        fd: RawFd,
        expires: Option<Instant>,
    ) {
        let mut shard = self.lock(own_shard());
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
        let mut shard = self.lock(own_shard());
        shard.entry(key).or_default().shared.push(Shared {
            sender,
            fd,
            streams: streams.clone(),
        });
        H2Stream(streams)
    }
}

/// A connection kept for `key` in `shard`: an HTTP/2 one with room for another request, else the
/// HTTP/1.1 connection idle the shortest time
fn take(shard: &mut Shard, key: u64, max_h2_streams: usize) -> Option<HttpSession> {
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
