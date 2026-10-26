//! Threads that don't steal work from each other (`work_stealing = false`): each runs a runtime
//! of its own, accepts connections, and serves them. A thread with more than twice (plus one) the
//! connections open of the least busy one hands a new connection over to it: connections that
//! last (kept alive, HTTP/2) are spread over the threads, while short ones (whose counts come and
//! go) stay where they're accepted.

use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::runtime::Handle;

/// A thread, and the connections it serves: on a cache line of its own, as each thread counts
/// its connections all the time (and only reads the others')
#[repr(align(128))]
struct Worker {
    handle: Handle,
    open: AtomicUsize,
}

/// The threads of a service
pub struct Workers {
    workers: Box<[Worker]>,
}

/// A connection served by a thread, counted until dropped
pub struct Open {
    workers: &'static Workers,
    index: usize,
}

impl Drop for Open {
    fn drop(&mut self) {
        if let Some(worker) = self.workers.workers.get(self.index) {
            worker.open.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl Workers {
    /// `threads` threads named after `name`, running until the server exits
    pub(super) fn start(threads: usize, name: &str) -> std::io::Result<Self> {
        let workers = (0..threads)
            .map(|index| {
                let handle = thread(&format!("{name}-{index}"))?;
                Ok(Worker {
                    handle,
                    open: AtomicUsize::new(0),
                })
            })
            .collect::<std::io::Result<_>>()?;
        Ok(Self { workers })
    }

    pub fn len(&self) -> usize {
        self.workers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }

    pub fn handle(&self, index: usize) -> Option<&Handle> {
        self.workers.get(index).map(|worker| &worker.handle)
    }

    /// The thread to serve a connection the thread `accepted` took: itself, unless it serves
    /// more than twice (plus one) the connections of the least busy thread
    pub fn serving(&self, accepted: usize) -> usize {
        let open = |worker: &Worker| worker.open.load(Ordering::Relaxed);
        let Some(mine) = self.workers.get(accepted).map(open) else {
            return accepted;
        };
        let least = self
            .workers
            .iter()
            .enumerate()
            .min_by_key(|(_, worker)| open(worker))
            .map(|(index, worker)| (index, open(worker)));
        match least {
            Some((index, fewest)) if mine > 2 * fewest + 1 => index,
            _ => accepted,
        }
    }

    /// A connection served by the thread `index`, counted until dropped
    pub fn open(&'static self, index: usize) -> Open {
        if let Some(worker) = self.workers.get(index) {
            worker.open.fetch_add(1, Ordering::Relaxed);
        }
        Open {
            workers: self,
            index,
        }
    }
}

/// A thread with a runtime of its own, running until the server exits
fn thread(name: &str) -> std::io::Result<Handle> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let handle = runtime.handle().clone();
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || runtime.block_on(std::future::pending::<()>()))?;
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::Workers;

    #[test]
    fn test_connections_are_spread_over_threads() {
        let workers: &'static Workers = Box::leak(Box::new(Workers::start(2, "test").unwrap()));
        // Short connections stay where they're accepted
        let first = workers.open(workers.serving(0));
        let second = workers.open(workers.serving(0));
        // Thread 0 serves two, thread 1 none: the next goes to thread 1
        assert_eq!(workers.serving(0), 1);
        let third = workers.open(1);
        // Within twice (plus one) of thread 1's: kept
        assert_eq!(workers.serving(0), 0);
        drop((first, second, third));
    }
}
