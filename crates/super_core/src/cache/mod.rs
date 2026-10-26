//! Responses served from a cache: what a request's cache state is ([`HttpCache`]), and where
//! responses are stored ([`Storage`]).
//!
//! A fresh response is served from the cache; a stale one is fetched again (and served if the
//! upstream fails within its `stale_if_error` window); one request at a time fetches a missing
//! response when a [`lock::CacheLock`] is set.

use std::time::SystemTime;

pub mod key;
pub mod lock;
pub mod memory;
mod meta;
pub mod storage;

pub use key::{CacheHashKey, CacheKey};
pub use memory::MemCache;
pub use meta::CacheMeta;
pub use storage::{HandleHit, HandleMiss, HitHandler, MissFinishType, MissHandler, Storage};

use lock::{CacheLock, WriteLock};

/// Why a response isn't cached
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoCacheReason {
    NeverEnabled,
    OriginNotCache,
    ResponseTooLarge,
    StorageError,
    InternalError,
    Custom(&'static str),
}

/// Whether a response is cached, and how
#[derive(Debug)]
pub enum RespCacheable {
    Cacheable(CacheMeta),
    Uncacheable(NoCacheReason),
}

/// A response found in the cache, treated as expired
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForcedFreshness {
    /// Fetched again, never served stale
    ForceExpired,
    /// Fetched again, served stale if the upstream fails (within its `stale_if_error` window)
    ForceExpiredServeStale { expired_at: Option<SystemTime> },
}

/// Where a request is in the cache
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePhase {
    Disabled,
    /// Enabled, not looked up yet
    Uninit,
    Hit,
    /// Missing (or stale): fetched from the upstream, and stored if cacheable
    Miss,
    /// The response isn't stored
    Bypass,
}

/// The cache state of a request
pub struct HttpCache {
    phase: CachePhase,
    storage: Option<&'static (dyn Storage + Sync)>,
    lock: Option<&'static CacheLock>,
    key: Option<CacheKey>,
    max_file_size: Option<usize>,
    /// Held while this request fetches its key's response
    pub(crate) write_lock: Option<WriteLock>,
    /// Stores the response as it's proxied
    pub(crate) miss_handler: Option<MissHandler>,
    pub(crate) written: usize,
    /// A stale response, served if the upstream fails
    pub(crate) stale: Option<(CacheMeta, HitHandler)>,
}

impl HttpCache {
    pub fn new() -> Self {
        Self {
            phase: CachePhase::Disabled,
            storage: None,
            lock: None,
            key: None,
            max_file_size: None,
            write_lock: None,
            miss_handler: None,
            written: 0,
            stale: None,
        }
    }

    /// Serves the request from `storage`, and stores its response there. With a `lock`, one
    /// request at a time fetches a missing response.
    pub fn enable(
        &mut self,
        storage: &'static (dyn Storage + Sync),
        lock: Option<&'static CacheLock>,
    ) {
        self.phase = CachePhase::Uninit;
        self.storage = Some(storage);
        self.lock = lock;
    }

    pub fn disable(&mut self) {
        self.phase = CachePhase::Disabled;
        self.miss_handler = None;
        self.write_lock = None;
    }

    pub fn enabled(&self) -> bool {
        self.phase != CachePhase::Disabled
    }

    pub fn phase(&self) -> CachePhase {
        self.phase
    }

    /// Responses larger than this aren't stored
    pub fn set_max_file_size_bytes(&mut self, size: usize) {
        self.max_file_size = Some(size);
    }

    pub fn max_file_size_bytes(&self) -> Option<usize> {
        self.max_file_size
    }

    /// The response is fetched from the upstream, and stored if cacheable
    pub fn cache_miss(&mut self) {
        if self.enabled() {
            self.phase = CachePhase::Miss;
        }
    }

    pub(crate) fn set_phase(&mut self, phase: CachePhase) {
        self.phase = phase;
    }

    pub fn cache_key(&self) -> Option<&CacheKey> {
        self.key.as_ref()
    }

    pub(crate) fn set_cache_key(&mut self, key: CacheKey) {
        self.key = Some(key);
    }

    pub fn storage(&self) -> Option<&'static (dyn Storage + Sync)> {
        self.storage
    }

    pub(crate) fn lock(&self) -> Option<&'static CacheLock> {
        self.lock
    }
}

impl Default for HttpCache {
    fn default() -> Self {
        Self::new()
    }
}
