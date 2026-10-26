//! Where cached responses are stored: looked up by key ([`Storage::lookup`]), read part by part
//! ([`HandleHit`]), and written as they're proxied ([`HandleMiss`])

use std::any::Any;

use async_trait::async_trait;
use bytes::Bytes;

use super::{CacheKey, CacheMeta};
use crate::Result;

/// How a response was stored
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissFinishType {
    /// Stored, with its size
    Created(usize),
    Appended(usize),
}

/// A storage of cached responses
#[async_trait]
pub trait Storage {
    /// The response stored for `key`, if any
    async fn lookup(&'static self, key: &CacheKey) -> Result<Option<(CacheMeta, HitHandler)>>;

    /// Stores `meta` for `key`: its body is written with the handler returned
    async fn get_miss_handler(
        &'static self,
        key: &CacheKey,
        meta: &CacheMeta,
    ) -> Result<MissHandler>;

    /// Removes what's stored for `key`. Returns whether something was.
    async fn purge(&'static self, key: &CacheKey) -> Result<bool>;

    /// Replaces the meta of what's stored for `key`
    async fn update_meta(&'static self, key: &CacheKey, meta: &CacheMeta) -> Result<bool>;

    fn as_any(&self) -> &(dyn Any + Send + Sync + 'static);
}

/// Reads a cached response's body
#[async_trait]
pub trait HandleHit {
    /// The next part of the body, `None` at its end
    async fn read_body(&mut self) -> Result<Option<Bytes>>;

    /// The body was served
    async fn finish(
        self: Box<Self>,
        storage: &'static (dyn Storage + Sync),
        key: &CacheKey,
    ) -> Result<()>;

    fn as_any(&self) -> &(dyn Any + Send + Sync);
}

/// Writes a response's body to the storage. Dropped without `finish`, the response isn't stored.
#[async_trait]
pub trait HandleMiss {
    async fn write_body(&mut self, data: Bytes, eof: bool) -> Result<()>;

    async fn finish(self: Box<Self>) -> Result<MissFinishType>;
}

pub type HitHandler = Box<dyn HandleHit + Send + Sync>;
pub type MissHandler = Box<dyn HandleMiss + Send + Sync>;
