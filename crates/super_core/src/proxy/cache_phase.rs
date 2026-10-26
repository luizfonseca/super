//! The cache's part of a request: served from the cache when fresh, else fetched (one request
//! at a time with a lock) and stored as it's proxied; a stale response is served when the
//! upstream fails within its `stale_if_error` window

use std::time::SystemTime;

use bytes::Bytes;

use super::{HttpProxy, ProxyHttp, Session};
use crate::{
    cache::{
        lock::Locked, CacheKey, CacheMeta, CachePhase, ForcedFreshness, HitHandler, RespCacheable,
    },
    http::ResponseHeader,
    Result,
};

/// Looks the request up: `true` once served from the cache
pub(super) async fn lookup<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
) -> Result<bool> {
    let key = proxy.inner.cache_key_callback(session, ctx)?;
    let Some(storage) = session.cache.storage() else {
        return Ok(false);
    };
    for waited in [false, true] {
        let found = match storage.lookup(&key).await {
            Ok(found) => found,
            Err(err) => {
                tracing::warn!("cache lookup failed: {err}");
                session.cache.disable();
                return Ok(false);
            }
        };
        if served(proxy, session, ctx, &key, found).await? {
            return Ok(true);
        }
        if waited || !lock(session, &key).await {
            break;
        }
    }
    session.cache.set_cache_key(key);
    proxy.inner.cache_miss(session, ctx);
    Ok(false)
}

/// Serves the response found in the cache, if any and fresh (and `cache_hit_filter` agrees): `true`
/// once served. A stale one is kept, to be served if the upstream fails.
async fn served<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    key: &CacheKey,
    found: Option<(CacheMeta, HitHandler)>,
) -> Result<bool> {
    let Some((meta, mut hit)) = found else {
        return Ok(false);
    };
    let fresh = meta.is_fresh(SystemTime::now());
    let forced = proxy
        .inner
        .cache_hit_filter(session, &meta, &mut hit, fresh, ctx)
        .await?;
    if fresh && forced.is_none() {
        session.cache.set_cache_key(key.clone());
        serve(proxy, session, ctx, &meta, hit).await?;
        return Ok(true);
    }
    // Fetched again; served if the upstream fails, unless it can't be
    if forced != Some(ForcedFreshness::ForceExpired) {
        session.cache.stale = Some((meta, hit));
    }
    Ok(false)
}

/// Takes the key's lock: `true` when another request fetched the response meanwhile (it may
/// be in the cache now)
async fn lock(session: &mut Session, key: &CacheKey) -> bool {
    let Some(lock) = session.cache.lock() else {
        return false;
    };
    match lock.lock(key) {
        Locked::Write(write) => {
            session.cache.write_lock = Some(write);
            false
        }
        Locked::Read(read) => {
            read.wait().await;
            true
        }
    }
}

/// Serves a cached response, through `response_filter` and `response_body_filter`
async fn serve<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    meta: &CacheMeta,
    mut hit: HitHandler,
) -> Result<()> {
    session.cache.set_phase(CachePhase::Hit);
    let mut header: ResponseHeader = meta.response_header().clone();
    proxy
        .inner
        .response_filter(session, &mut header, ctx)
        .await?;
    let head = session.req_header().method == http::Method::HEAD;
    session
        .write_response_header(Box::new(header), head)
        .await?;
    if !head {
        serve_body(proxy, session, ctx, &mut hit).await?;
    }
    if let (Some(storage), Some(key)) = (session.cache.storage(), session.cache.cache_key()) {
        hit.finish(storage, key).await?;
    }
    Ok(())
}

/// The body of a cached response, through `response_body_filter` (and its pace)
async fn serve_body<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    hit: &mut HitHandler,
) -> Result<()> {
    loop {
        let mut data = hit.read_body().await?;
        let end = data.is_none();
        let wait = proxy
            .inner
            .response_body_filter(session, &mut data, end, ctx)?;
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
        }
        session.write_response_body(data, end).await?;
        if end {
            return Ok(());
        }
    }
}

/// Serves the stale response kept, if the request failed before anything was sent and it's
/// within its `stale_if_error` window. `None` when it isn't served.
pub(super) async fn serve_stale<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
) -> Option<Result<()>> {
    if session.response_written().is_some() {
        return None;
    }
    let (meta, hit) = session.cache.stale.take()?;
    if !meta.serve_stale_if_error(SystemTime::now()) {
        return None;
    }
    session.cache.miss_handler = None;
    Some(serve(proxy, session, ctx, &meta, hit).await)
}

/// Stores the upstream's response as it's proxied, if `response_cache_filter` says it's
/// cacheable
pub(super) async fn admit<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    header: &ResponseHeader,
) {
    if session.cache.phase() != CachePhase::Miss {
        return;
    }
    let meta = match proxy.inner.response_cache_filter(session, header, ctx) {
        Ok(RespCacheable::Cacheable(meta)) => meta,
        Ok(RespCacheable::Uncacheable(_)) | Err(_) => return bypass(session),
    };
    let length = header
        .headers
        .get(http::header::CONTENT_LENGTH)
        .and_then(|length| length.to_str().ok()?.parse::<usize>().ok());
    let max = session.cache.max_file_size_bytes();
    if length.zip(max).is_some_and(|(length, max)| length > max) {
        return bypass(session);
    }
    let (Some(storage), Some(key)) = (session.cache.storage(), session.cache.cache_key()) else {
        return bypass(session);
    };
    match storage.get_miss_handler(key, &meta).await {
        Ok(handler) => session.cache.miss_handler = Some(handler),
        Err(err) => {
            tracing::warn!("can't store the response in the cache: {err}");
            bypass(session);
        }
    }
}

/// The response isn't stored: requests waiting for it fetch it themselves
fn bypass(session: &mut Session) {
    session.cache.set_phase(CachePhase::Bypass);
    session.cache.write_lock = None;
}

/// Stores a part of the response being admitted (the whole response with `end`)
pub(super) async fn write(session: &mut Session, data: Option<&Bytes>, end: bool) {
    let max = session.cache.max_file_size_bytes();
    let cache = &mut session.cache;
    let Some(handler) = &mut cache.miss_handler else {
        return;
    };
    let length = data.map_or(0, Bytes::len);
    cache.written += length;
    if max.is_some_and(|max| cache.written > max) {
        // Too large: left out (and nothing is kept of it)
        cache.miss_handler = None;
        cache.write_lock = None;
        return;
    }
    if let Some(data) = data {
        if let Err(err) = handler.write_body(data.clone(), end).await {
            tracing::warn!("can't store the response in the cache: {err}");
            cache.miss_handler = None;
            cache.write_lock = None;
            return;
        }
    }
    if !end {
        return;
    }
    if let Some(handler) = cache.miss_handler.take() {
        if let Err(err) = handler.finish().await {
            tracing::warn!("can't store the response in the cache: {err}");
        }
    }
    cache.write_lock = None;
}
