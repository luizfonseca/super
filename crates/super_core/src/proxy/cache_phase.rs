//! The cache's part of a request: served from the cache when fresh, else fetched (one request
//! at a time with a lock) and stored as it's proxied. A stale response is revalidated with the
//! upstream (a `304` renews it), served to the other requests while one fetches it again (within
//! its `stale_while_revalidate` window), and served when the upstream fails (within its
//! `stale_if_error` window). Clients get `304`s and ranges from the cache too.

use std::time::SystemTime;

use bytes::Bytes;

use super::{
    conditional::{self, Range},
    headers, HttpProxy, ProxyHttp, Session,
};
use crate::{
    cache::{
        lock::Locked, CacheKey, CacheMeta, CachePhase, ForcedFreshness, HitHandler, RespCacheable,
    },
    http::ResponseHeader,
    Result,
};

/// What was found in the cache for a request
enum Found {
    /// Served from it
    Served,
    /// Stale: this request fetches it again (while the others get the stale response)
    Refresh,
    /// Nothing to serve: fetched (once the lock is taken)
    Missing,
}

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
        match decide(proxy, session, ctx, &key, found).await? {
            Found::Served => return Ok(true),
            Found::Refresh => break,
            Found::Missing => {}
        }
        if waited || !lock(session, &key).await {
            break;
        }
    }
    session.cache.set_cache_key(key);
    proxy.inner.cache_miss(session, ctx);
    Ok(false)
}

/// What to do with the response found in the cache, if any: serve it when it's fresh (and
/// `cache_hit_filter` agrees). A stale one is kept, to be revalidated with the upstream, and
/// served if the upstream fails; within its `stale_while_revalidate` window, one request fetches
/// it again while the others are served it.
async fn decide<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    key: &CacheKey,
    found: Option<(CacheMeta, HitHandler)>,
) -> Result<Found> {
    let Some((meta, mut hit)) = found else {
        return Ok(Found::Missing);
    };
    let now = SystemTime::now();
    let fresh = meta.is_fresh(now);
    let forced = proxy
        .inner
        .cache_hit_filter(session, &meta, &mut hit, fresh, ctx)
        .await?;
    if fresh && forced.is_none() {
        session.cache.set_cache_key(key.clone());
        serve(proxy, session, ctx, &meta, hit).await?;
        return Ok(Found::Served);
    }
    if forced == Some(ForcedFreshness::ForceExpired) {
        return Ok(Found::Missing);
    }
    let while_revalidating = meta.serve_stale_while_revalidate(now);
    session.cache.stale = Some((meta, hit));
    let lock = session.cache.lock().filter(|_| while_revalidating);
    let Some(lock) = lock else {
        return Ok(Found::Missing);
    };
    match lock.lock(key) {
        Locked::Write(write) => {
            session.cache.write_lock = Some(write);
            Ok(Found::Refresh)
        }
        // Another request fetches it again: this one gets the stale response
        Locked::Read(_) => {
            let Some((meta, hit)) = session.cache.stale.take() else {
                return Ok(Found::Missing);
            };
            session.cache.set_cache_key(key.clone());
            serve(proxy, session, ctx, &meta, hit).await?;
            Ok(Found::Served)
        }
    }
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

/// Serves a cached response, through `response_filter` and `response_body_filter`: a `304` to
/// a client whose copy is current, the part of the body it asks for (`206`), or the whole body
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
    let request = session.req_header();
    if conditional::not_modified(request, &header) {
        conditional::to_not_modified(&mut header);
        return session.write_response_header(header, true).await;
    }
    let range = conditional::range(request, &header);
    if let Some(range) = &range {
        conditional::to_range(&mut header, range);
    }
    let head = session.req_header().method == http::Method::HEAD;
    let empty = head || matches!(range, Some(Range::Unsatisfiable { .. }));
    session
        .write_response_header(header, empty)
        .await?;
    if empty {
        return Ok(());
    }
    if let Some(Range::Part { start, end, .. }) = range {
        return serve_part(proxy, session, ctx, &mut hit, start, end).await;
    }
    serve_body(proxy, session, ctx, &mut hit).await?;
    // The whole body was read (a storage may keep what it read)
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

/// The bytes `start` to `end` (included) of a cached body, through `response_body_filter`
async fn serve_part<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    hit: &mut HitHandler,
    start: u64,
    end: u64,
) -> Result<()> {
    let mut offset = 0u64;
    while let Some(data) = hit.read_body().await? {
        let from = offset;
        offset += data.len() as u64;
        if offset <= start {
            continue;
        }
        let first = usize::try_from(start.saturating_sub(from)).unwrap_or(usize::MAX);
        let last = usize::try_from((end + 1 - from).min(data.len() as u64)).unwrap_or(0);
        let mut part = Some(data.slice(first.min(last)..last));
        let done = offset > end;
        if let Some(wait) = proxy
            .inner
            .response_body_filter(session, &mut part, done, ctx)?
        {
            tokio::time::sleep(wait).await;
        }
        session.write_response_body(part, done).await?;
        if done {
            return Ok(());
        }
    }
    session.write_response_body(None, true).await
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
    // Whole successful responses only (not a part of a body, a `304` to a client's question, nor
    // a `HEAD`'s, which has no body)
    let whole = header.status.is_success()
        && header.status != http::StatusCode::PARTIAL_CONTENT
        && session.req_header().method != http::Method::HEAD;
    if !whole {
        return bypass(session);
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

/// Asks the upstream whether the stale response kept for this request changed (when the
/// request is fetched again): `true` when it asked
pub(super) fn ask(session: &mut Session, request: &mut crate::http::RequestHeader) -> bool {
    if session.cache.phase() != CachePhase::Miss {
        return false;
    }
    let Some((meta, _)) = &session.cache.stale else {
        return false;
    };
    conditional::ask_upstream(request, meta.headers())
}

/// The upstream says the stale response didn't change (`304`): it's renewed (its header updated
/// by the `304`'s, its freshness by `response_cache_filter`), and served
pub(super) async fn revalidated<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    mut not_modified: ResponseHeader,
) -> Result<()> {
    headers::downstream_response(&mut not_modified, false);
    proxy
        .inner
        .upstream_response_filter(session, &mut not_modified, ctx)
        .await?;
    let Some((stale, hit)) = session.cache.stale.take() else {
        return Err(crate::Error::explain(
            crate::ErrorType::InternalError,
            "revalidated without a stale response",
        ));
    };
    let merged = conditional::merge(stale.response_header(), &not_modified);
    let renewed = match proxy.inner.response_cache_filter(session, &merged, ctx) {
        Ok(RespCacheable::Cacheable(meta)) => Some(meta),
        _ => None,
    };
    let meta = renewed.clone().unwrap_or_else(|| {
        CacheMeta::new(
            stale.fresh_until(),
            stale.created(),
            stale.stale_while_revalidate_sec(),
            stale.stale_if_error_sec(),
            merged,
        )
    });
    let stored = (renewed, session.cache.storage(), session.cache.cache_key());
    if let (Some(renewed), Some(storage), Some(key)) = stored {
        if let Err(err) = storage.update_meta(key, &renewed).await {
            tracing::warn!("can't renew the cached response: {err}");
        }
    }
    session.cache.write_lock = None;
    serve(proxy, session, ctx, &meta, hit).await
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
