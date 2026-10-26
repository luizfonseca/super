//! A storage in memory, without a limit (as Pingora's `MemCache`)

use std::{
    any::Any,
    collections::HashMap,
    sync::{Arc, PoisonError, RwLock},
};

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};

use super::{
    key::HashBinary, CacheHashKey, CacheKey, CacheMeta, HandleHit, HandleMiss, HitHandler,
    MissFinishType, MissHandler, Storage,
};
use crate::Result;

type Entries = Arc<RwLock<HashMap<HashBinary, (CacheMeta, Bytes)>>>;

#[derive(Default)]
pub struct MemCache {
    entries: Entries,
}

impl MemCache {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Storage for MemCache {
    async fn lookup(&'static self, key: &CacheKey) -> Result<Option<(CacheMeta, HitHandler)>> {
        let entries = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        let Some((meta, body)) = entries.get(&key.primary_bin()) else {
            return Ok(None);
        };
        let hit: HitHandler = Box::new(Hit(Some(body.clone())));
        Ok(Some((meta.clone(), hit)))
    }

    async fn get_miss_handler(
        &'static self,
        key: &CacheKey,
        meta: &CacheMeta,
    ) -> Result<MissHandler> {
        Ok(Box::new(Miss {
            key: key.primary_bin(),
            meta: meta.clone(),
            body: BytesMut::new(),
            entries: self.entries.clone(),
        }))
    }

    async fn purge(&'static self, key: &CacheKey) -> Result<bool> {
        let mut entries = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        Ok(entries.remove(&key.primary_bin()).is_some())
    }

    async fn update_meta(&'static self, key: &CacheKey, meta: &CacheMeta) -> Result<bool> {
        let mut entries = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = entries.get_mut(&key.primary_bin()) else {
            return Ok(false);
        };
        entry.0 = meta.clone();
        Ok(true)
    }

    fn as_any(&self) -> &(dyn Any + Send + Sync + 'static) {
        self
    }
}

/// The body, whole
struct Hit(Option<Bytes>);

#[async_trait]
impl HandleHit for Hit {
    async fn read_body(&mut self) -> Result<Option<Bytes>> {
        Ok(self.0.take())
    }

    async fn finish(
        self: Box<Self>,
        _storage: &'static (dyn Storage + Sync),
        _key: &CacheKey,
    ) -> Result<()> {
        Ok(())
    }

    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

struct Miss {
    key: HashBinary,
    meta: CacheMeta,
    body: BytesMut,
    entries: Entries,
}

#[async_trait]
impl HandleMiss for Miss {
    async fn write_body(&mut self, data: Bytes, _eof: bool) -> Result<()> {
        self.body.extend_from_slice(&data);
        Ok(())
    }

    async fn finish(self: Box<Self>) -> Result<MissFinishType> {
        let size = self.body.len();
        let mut entries = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        entries.insert(self.key, (self.meta, self.body.freeze()));
        Ok(MissFinishType::Created(size))
    }
}
