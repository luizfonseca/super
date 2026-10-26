//! One request at a time fetches a response missing from the cache: the others wait for it
//! (for a while), then find it in the cache

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
    time::Duration,
};

use tokio::sync::watch;

use super::{key::HashBinary, CacheHashKey, CacheKey};

pub struct CacheLock {
    timeout: Duration,
    /// The keys being fetched: their writers' end (closed once fetched)
    writing: Mutex<HashMap<HashBinary, watch::Receiver<()>>>,
}

/// What a request got for its key
pub enum Locked {
    /// It fetches the response
    Write(WriteLock),
    /// Another request fetches it
    Read(ReadLock),
}

/// The right to fetch a key's response, until dropped
pub struct WriteLock {
    lock: &'static CacheLock,
    key: HashBinary,
    _writing: watch::Sender<()>,
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        self.lock.writing().remove(&self.key);
    }
}

/// Waits for another request to fetch a key's response
pub struct ReadLock {
    writing: watch::Receiver<()>,
    timeout: Duration,
}

impl ReadLock {
    /// Returns once the other request is done, or after the lock's timeout
    pub async fn wait(mut self) {
        let done = self.writing.wait_for(|()| false);
        let _ = tokio::time::timeout(self.timeout, done).await;
    }
}

impl CacheLock {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            writing: Mutex::default(),
        }
    }

    fn writing(&self) -> std::sync::MutexGuard<'_, HashMap<HashBinary, watch::Receiver<()>>> {
        self.writing.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn lock(&'static self, key: &CacheKey) -> Locked {
        let key = key.primary_bin();
        let mut writing = self.writing();
        if let Some(writer) = writing.get(&key) {
            return Locked::Read(ReadLock {
                writing: writer.clone(),
                timeout: self.timeout,
            });
        }
        let (sender, receiver) = watch::channel(());
        writing.insert(key, receiver);
        Locked::Write(WriteLock {
            lock: self,
            key,
            _writing: sender,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::LazyLock, time::Duration};

    use super::{CacheLock, Locked};
    use crate::cache::CacheKey;

    static LOCK: LazyLock<CacheLock> = LazyLock::new(|| CacheLock::new(Duration::from_secs(5)));

    #[tokio::test]
    async fn test_readers_wait_for_the_writer() {
        let key = CacheKey::new("/a", "a.test");
        let Locked::Write(writer) = LOCK.lock(&key) else {
            panic!("the first request writes");
        };
        let Locked::Read(reader) = LOCK.lock(&key) else {
            panic!("the next one waits");
        };
        let started = tokio::time::Instant::now();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(writer);
        });
        reader.wait().await;
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(50) && waited < Duration::from_secs(1));
        assert!(matches!(LOCK.lock(&key), Locked::Write(_)));
    }
}
