//! Connections kept for the next requests to the same upstream: idle HTTP/1.1 connections (one
//! request at a time), and open HTTP/2 connections (shared by up to `max_h2_streams` requests).
//! Taking one allocates nothing.
//!
//! Each thread keeps the connections it opens in a shard of its own: threads proxying to the same
//! upstream don't wait for each other's lock, and a connection's requests run on the thread its
//! driver runs on (no thread wakes another for each request).
//!
//! A thread without an idle connection to an upstream borrows another thread's, as Pingora's
//! shared pool would, rather than open one: an upstream little used, or a thread seldom sending
//! it requests, doesn't get a connection per thread. A borrowed connection goes back to its
//! thread after the request. A thread that borrowed less than a second ago opens its own
//! instead: a thread busy with an upstream has connections of its own (each part of a body on a
//! borrowed one would wake its thread).

use std::{
    cell::Cell,
    collections::VecDeque,
    os::fd::RawFd,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, PoisonError,
    },
};

use hyper::client::conn::{http1, http2};

use crate::clock::Coarse;

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
pub(crate) fn own_shard() -> usize {
    SHARD.with(|shard| {
        shard.get().unwrap_or_else(|| {
            let index = NEXT_SHARD.fetch_add(1, Ordering::Relaxed) % SHARDS;
            shard.set(Some(index));
            index
        })
    })
}

/// A thread borrowing connections more often than this opens its own
const BORROWING: std::time::Duration = std::time::Duration::from_secs(1);

/// Idle connections kept per upstream, at most (shared by the threads, as Pingora's pool)
const MAX_IDLE: usize = 128;

struct Idle {
    sender: http1::SendRequest<UpstreamBody>,
    fd: RawFd,
    expires: Option<Coarse>,
}

impl Idle {
    /// Expired, or closed by the upstream
    fn gone(&self, now: Coarse) -> bool {
        self.expires.is_some_and(|expires| expires <= now) || self.sender.is_closed()
    }
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
    /// When this thread last borrowed another thread's connection to it
    borrowed_at: Option<Coarse>,
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

    /// A connection kept for `key`: one of this thread's, else (when it didn't borrow one less
    /// than `BORROWING` ago) one borrowed from another thread whose shard isn't busy
    pub fn checkout(&self, key: u64, max_h2_streams: usize) -> Option<HttpSession> {
        let own = own_shard();
        {
            let mut shard = self.lock(own);
            if let Some(session) = take(&mut shard, own, key, max_h2_streams) {
                return Some(session);
            }
            let now = Coarse::now();
            let upstream = shard.entry(key).or_default();
            let busy = upstream
                .borrowed_at
                .is_some_and(|at| now.since(at) < BORROWING);
            if busy {
                return None;
            }
            upstream.borrowed_at = Some(now);
        }
        // The shards threads were given
        let used = NEXT_SHARD.load(Ordering::Relaxed).min(SHARDS);
        (1..used).find_map(|offset| {
            let home = (own + offset) % used;
            let mut shard = self.shards.get(home)?.try_lock().ok()?;
            take(&mut shard, home, key, max_h2_streams)
        })
    }

    /// Keeps an idle HTTP/1.1 connection in the shard of its thread (`home`), for `idle_timeout`.
    /// The connections there that expired or were closed go (oldest first), and so does the
    /// oldest when its thread keeps its share of `MAX_IDLE` already.
    pub fn keep(
        &self,
        home: usize,
        key: u64,
        sender: http1::SendRequest<UpstreamBody>,
        fd: RawFd,
        idle_timeout: Option<std::time::Duration>,
    ) {
        let now = Coarse::now();
        let expires = idle_timeout.map(|timeout| now.after(timeout));
        // Each thread's share of the connections kept per upstream
        let threads = NEXT_SHARD.load(Ordering::Relaxed).clamp(1, SHARDS);
        let most = MAX_IDLE.div_ceil(threads);
        let mut shard = self.lock(home);
        let upstream = shard.entry(key).or_default();
        while upstream.idle.front().is_some_and(|idle| idle.gone(now)) {
            upstream.idle.pop_front();
        }
        if upstream.idle.len() >= most {
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

/// A connection kept for `key` in `shard` (that of thread `home`): an HTTP/2 one with room for
/// another request, else the HTTP/1.1 connection idle the shortest time
fn take(shard: &mut Shard, home: usize, key: u64, max_h2_streams: usize) -> Option<HttpSession> {
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
            home,
            fd: shared.fd,
        });
    }
    let now = Coarse::now();
    while let Some(idle) = upstream.idle.pop_back() {
        if idle.gone(now) {
            continue;
        }
        return Some(HttpSession {
            sender: Sender::H1(idle.sender),
            key,
            home,
            fd: idle.fd,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use hyper_util::rt::TokioIo;

    use super::{own_shard, Pool};
    use crate::upstreams::{body::UpstreamBody, connector::Sender};

    /// An idle HTTP/1.1 connection, to a listener that never answers
    async fn connection() -> hyper::client::conn::http1::SendRequest<UpstreamBody> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
            drop(socket);
        });
        let socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (sender, driver) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
            .await
            .unwrap();
        tokio::spawn(driver);
        sender
    }

    #[tokio::test]
    async fn test_threads_borrow_each_others_connections() {
        let pool = std::sync::Arc::new(Pool::new());
        let home = own_shard();
        pool.keep(home, 7, connection().await, 0, None);

        // Another thread, without connections of its own, borrows it
        let borrowed = {
            let pool = pool.clone();
            std::thread::spawn(move || {
                let other = own_shard();
                let session = pool.checkout(7, 1).unwrap();
                assert_ne!(other, session.home);
                let Sender::H1(sender) = session.sender else {
                    panic!("an HTTP/1.1 connection");
                };
                // Released: back to its thread's shard
                pool.keep(session.home, 7, sender, 0, None);
                session.home
            })
            .join()
            .unwrap()
        };
        assert_eq!(borrowed, home);
        assert!(pool.checkout(7, 1).is_some());
        assert!(pool.checkout(7, 1).is_none());
    }

    #[tokio::test]
    async fn test_busy_threads_use_their_own_connections() {
        let pool = std::sync::Arc::new(Pool::new());
        pool.keep(own_shard(), 8, connection().await, 0, None);
        pool.keep(own_shard(), 8, connection().await, 0, None);

        let borrowed = {
            let pool = pool.clone();
            std::thread::spawn(move || {
                // Borrowed once, then (less than a second later) its own is opened
                let first = pool.checkout(8, 1).is_some();
                (first, pool.checkout(8, 1).is_some())
            })
            .join()
            .unwrap()
        };
        assert_eq!(borrowed, (true, false));
        assert!(pool.checkout(8, 1).is_some());
    }
}
