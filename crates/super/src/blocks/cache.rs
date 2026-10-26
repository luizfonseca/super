//! `cache { ... }`: serves responses from memory or disk

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use once_cell::sync::Lazy;
use super_core::cache::lock::CacheLock;
use super_core::proxy::Session;

use crate::{
    cache::disk::storage::DiskCache,
    config::{RouteCache, RouteCacheType},
    stores,
};

use super_plugins::RequestCtx;

use super::Middleware;

static STORAGE_MEM_CACHE: Lazy<super_core::cache::MemCache> =
    Lazy::new(super_core::cache::MemCache::new);
static STORAGE_CACHE: Lazy<DiskCache> = Lazy::new(DiskCache::new);
static CACHE_LOCK: Lazy<CacheLock> = Lazy::new(|| CacheLock::new(Duration::from_secs(1)));

fn storage(cache_type: &RouteCacheType) -> &'static (dyn super_core::cache::Storage + Sync) {
    match cache_type {
        RouteCacheType::Disk => &*STORAGE_CACHE,
        RouteCacheType::MemCache => &*STORAGE_MEM_CACHE,
    }
}

pub(super) struct Cache {
    cache: RouteCache,
    /// Where the disk cache is stored
    path: String,
}

impl Cache {
    /// `None` when the cache isn't enabled
    pub(super) fn new(cache: &RouteCache) -> Option<Self> {
        cache.enabled.unwrap_or(false).then(|| Self {
            cache: cache.clone(),
            path: cache.path.to_string_lossy().into_owned(),
        })
    }
}

#[async_trait]
impl Middleware for Cache {
    async fn request(&self, session: &mut Session, ctx: &mut dyn RequestCtx) -> Result<bool> {
        stores::insert_cache_routing(ctx.host(), &self.path);
        session
            .cache
            .enable(storage(&self.cache.cache_type), Some(&*CACHE_LOCK));
        Ok(false)
    }
}
