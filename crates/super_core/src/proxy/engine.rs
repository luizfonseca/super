//! A request's phases, from the client's request to the logging: it's answered by
//! `request_filter`, from the cache, or by an upstream (tried again as errors allow)

use std::sync::Arc;

use super::{cache_phase, exchange, HttpProxy, ProxyHttp, Session};
use crate::{
    protocols::http::ServerSession,
    upstreams::{connector::HttpSession, peer::HttpPeer},
    ErrorSource, Result,
};

/// Attempts at reaching an upstream, at most (Pingora's)
const MAX_RETRIES: usize = 16;

/// Handles the request of `downstream`, then logs it
pub(super) async fn process<SV: ProxyHttp>(proxy: &Arc<HttpProxy<SV>>, downstream: ServerSession) {
    let mut session = Session::new(downstream);
    let mut ctx = proxy.inner.new_ctx();
    let error = handle(proxy, &mut session, &mut ctx).await.err();
    if let Some(error) = &error {
        fail(proxy, &mut session, &mut ctx, error).await;
    }
    proxy
        .inner
        .logging(&mut session, error.as_deref(), &mut ctx)
        .await;
    if error.is_none() {
        // A response whose end wasn't written
        let _ = session.downstream_session.finish_body().await;
    }
}

async fn handle<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
) -> Result<()> {
    // What the client accepts, when compression is on (a block may turn it on later, and
    // tell it then)
    let compression = session.downstream_modules_ctx.compression();
    if compression.is_enabled() {
        compression.request_filter(session.downstream_session.req_header());
    }
    if proxy.inner.request_filter(session, ctx).await? {
        return Ok(());
    }
    if session.cache.enabled() && cache_phase::lookup(proxy, session, ctx).await? {
        return Ok(());
    }
    proxy_upstream(proxy, session, ctx).await
}

/// Sends the request to the upstream `upstream_peer` chooses, again as long as errors allow
async fn proxy_upstream<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
) -> Result<()> {
    let mut retries = 0;
    loop {
        let peer = proxy.inner.upstream_peer(session, ctx).await?;
        let Some((connection, reused)) = connect(proxy, session, ctx, &peer, &mut retries).await?
        else {
            continue;
        };
        let fd = connection.fd;
        proxy
            .inner
            .connected_to_upstream(session, reused, &peer, fd, None, ctx)
            .await?;
        let Err(e) = exchange::proxy(proxy, session, ctx, &peer, connection).await else {
            return Ok(());
        };
        let e = if *e.esource() == ErrorSource::Downstream {
            e
        } else {
            proxy
                .inner
                .error_while_proxy(&peer, session, e, ctx, reused)
        };
        let again = e.retry()
            && retries < MAX_RETRIES
            && session.response_written().is_none()
            && !session.retry_buffer_truncated();
        if !again {
            return Err(e);
        }
        tracing::debug!("trying again: {e}");
        retries += 1;
    }
}

/// A connection to `peer`, or `None` to try again (as `fail_to_connect` allows)
async fn connect<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    peer: &HttpPeer,
    retries: &mut usize,
) -> Result<Option<(HttpSession, bool)>> {
    let e = match proxy.connector.get_http_session(peer).await {
        Ok(connected) => return Ok(Some(connected)),
        Err(e) => proxy.inner.fail_to_connect(session, peer, ctx, e),
    };
    if !e.retry() || *retries >= MAX_RETRIES {
        return Err(e);
    }
    *retries += 1;
    Ok(None)
}

/// The request failed: served stale from the cache when allowed, else answered by
/// `fail_to_proxy`
async fn fail<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    error: &crate::Error,
) {
    if let Some(served) = cache_phase::serve_stale(proxy, session, ctx).await {
        match served {
            Ok(()) => return,
            Err(err) => tracing::debug!("serving a stale response failed: {err}"),
        }
    }
    let failure = proxy.inner.fail_to_proxy(session, error, ctx).await;
    if !failure.can_reuse_downstream {
        session.set_keepalive(None);
    }
    if failure.error_code > 0 {
        tracing::error!("{error}");
    }
}
