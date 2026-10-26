//! A request's phases, from the client's request to the logging: it's answered by
//! `request_filter`, from the cache, or by an upstream (tried again as errors allow)

use std::sync::Arc;

use super::{cache_phase, exchange, HttpProxy, ProxyHttp, Session};
use crate::{
    protocols::{http::ServerSession, ALPN},
    upstreams::{connector::HttpSession, peer::HttpPeer},
    ErrorSource, Result,
};

/// Attempts at reaching an upstream, at most (Pingora's)
const MAX_RETRIES: usize = 16;

/// Handles the request of `downstream`, then logs it
pub(super) async fn process<SV: ProxyHttp>(proxy: &Arc<HttpProxy<SV>>, downstream: ServerSession) {
    let mut session = Session::new(downstream);
    let mut ctx = proxy.inner.new_ctx();
    let mut progress = Progress::default();
    let error = handle(proxy, &mut session, &mut ctx, &mut progress)
        .await
        .err();
    if let Some(error) = &error {
        fail(proxy, &mut session, &mut ctx, error, &progress).await;
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

/// How far a request went, for its error log
#[derive(Default)]
struct Progress {
    /// Past `request_filter`
    proxying: bool,
    /// Attempts at reaching an upstream
    tries: usize,
}

async fn handle<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    progress: &mut Progress,
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
    progress.proxying = true;
    if session.cache.enabled() && cache_phase::lookup(proxy, session, ctx).await? {
        return Ok(());
    }
    proxy_upstream(proxy, session, ctx, progress).await
}

/// Sends the request to the upstream `upstream_peer` chooses, again as long as errors allow
async fn proxy_upstream<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    progress: &mut Progress,
) -> Result<()> {
    let mut retries = 0;
    loop {
        progress.tries = retries + 1;
        let mut peer = proxy.inner.upstream_peer(session, ctx).await?;
        // Switching protocols is HTTP/1.1's: an HTTP/2 upstream would take it as a plain request
        if session.is_upgrade_req() {
            peer.options.alpn = ALPN::H1;
        }
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
    progress: &Progress,
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
    // As Pingora logs them, for whoever looks for these lines
    let (code, summary) = (failure.error_code, summary(session));
    if progress.proxying {
        let tries = progress.tries;
        tracing::error!(
            "Fail to proxy: {error}, status: {code}, tries: {tries}, retry: false, {summary}"
        );
    } else {
        tracing::error!("Fail to filter request: {error}, status: {code}, {summary}");
    }
}

/// The request, in error logs: `GET /path?query, Host: host` (as Pingora's)
fn summary(session: &Session) -> String {
    let request = session.req_header();
    let host = request
        .headers
        .get(http::header::HOST)
        .map(|host| String::from_utf8_lossy(host.as_bytes()))
        .unwrap_or_default();
    let path = String::from_utf8_lossy(request.raw_path());
    format!("{} {path}, Host: {host}", request.method)
}
