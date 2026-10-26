//! Proxying requests to upstreams, in Pingora's phases ([`ProxyHttp`]):
//!
//! 1. `request_filter`: the request, before anything (it can be answered there)
//! 2. the cache: `cache_key_callback`, `cache_hit_filter`, `cache_miss` (when it's enabled)
//! 3. `upstream_peer`: where it goes; `fail_to_connect` when it can't be reached, then
//!    `connected_to_upstream`
//! 4. `upstream_request_filter`: the request sent; `request_body_filter` for each part of its body
//! 5. `upstream_response_filter`: the upstream's response, before it's cached
//!    (`response_cache_filter`); `response_filter`: the response sent to the client;
//!    `response_body_filter` for each part of its body
//! 6. `error_while_proxy` and `fail_to_proxy` when something fails, then `logging`, always
//!
//! Requests are tried again (on another upstream, as `upstream_peer` chooses) when an error
//! says so, as long as nothing was sent to the client and the body sent can be sent again.

use std::{future::Future, os::fd::RawFd, sync::Arc, time::Duration};

use bytes::Bytes;

use crate::{
    apps::{HttpServerApp, HttpServerOptions},
    cache::{CacheKey, CacheMeta, ForcedFreshness, HitHandler, NoCacheReason, RespCacheable},
    http::{RequestHeader, ResponseHeader},
    protocols::{http::ServerSession, Digest},
    server::{configuration::ServerConf, ShutdownWatch},
    upstreams::{connector::Connector, peer::HttpPeer},
    Error, ErrorSource, ErrorType, Result,
};

mod cache_phase;
mod conditional;
mod engine;
mod exchange;
mod headers;
mod session;
mod tunnel;
mod upload;

pub use headers::set_target;
pub use session::Session;

/// What [`ProxyHttp::fail_to_proxy`] answered
pub struct FailToProxy {
    /// The status answered (`0`: none, the client is gone)
    pub error_code: u16,
    /// Whether the client's connection can take another request
    pub can_reuse_downstream: bool,
}

/// The phases of a request, as Pingora's. Only `new_ctx` and `upstream_peer` have to be
/// written: the other phases change nothing by default.
pub trait ProxyHttp: Send + Sync + 'static {
    /// What the phases of a request share
    type CTX: Send + Sync;

    fn new_ctx(&self) -> Self::CTX;

    /// Where the request goes
    fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> impl Future<Output = Result<Box<HttpPeer>>> + Send;

    /// The request, before anything. `true` when it was answered here.
    fn request_filter(
        &self,
        _session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> impl Future<Output = Result<bool>> + Send {
        async { Ok(false) }
    }

    /// Each part of the request body, as it's sent to the upstream (the last one with
    /// `end_of_stream`; a request without a body gets one call, without a part)
    fn request_body_filter(
        &self,
        _session: &mut Session,
        _body: &mut Option<Bytes>,
        _end_of_stream: bool,
        _ctx: &mut Self::CTX,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }

    /// The request sent to the upstream
    fn upstream_request_filter(
        &self,
        _session: &mut Session,
        _upstream_request: &mut RequestHeader,
        _ctx: &mut Self::CTX,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }

    /// The upstream's response, before it's cached. An error that can be retried tries the
    /// request again.
    fn upstream_response_filter(
        &self,
        _session: &mut Session,
        _upstream_response: &mut ResponseHeader,
        _ctx: &mut Self::CTX,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }

    /// The response sent to the client (from the upstream, or the cache)
    fn response_filter(
        &self,
        _session: &mut Session,
        _upstream_response: &mut ResponseHeader,
        _ctx: &mut Self::CTX,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }

    /// Each part of the response body sent to the client: how long to wait before sending it
    ///
    /// # Errors
    ///
    /// An error ends the request.
    fn response_body_filter(
        &self,
        _session: &mut Session,
        _body: &mut Option<Bytes>,
        _end_of_stream: bool,
        _ctx: &mut Self::CTX,
    ) -> Result<Option<Duration>> {
        Ok(None)
    }

    /// The upstream couldn't be connected to: `e`, maybe to try again
    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        _ctx: &mut Self::CTX,
        e: Box<Error>,
    ) -> Box<Error> {
        e
    }

    /// Connected to the upstream (`reused`: a connection kept from a previous request)
    fn connected_to_upstream(
        &self,
        _session: &mut Session,
        _reused: bool,
        _peer: &HttpPeer,
        _fd: RawFd,
        _digest: Option<&Digest>,
        _ctx: &mut Self::CTX,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }

    /// Proxying failed after connecting: `e`, maybe to try again
    fn error_while_proxy(
        &self,
        peer: &HttpPeer,
        session: &mut Session,
        e: Box<Error>,
        _ctx: &mut Self::CTX,
        client_reused: bool,
    ) -> Box<Error> {
        let mut e = e.more_context(format!("Peer: {peer}"));
        if session.retry_buffer_truncated() {
            e.set_retry(false);
        } else {
            e.retry.decide_reuse(client_reused);
        }
        e
    }

    /// The request couldn't be proxied: answers the client (by default, with the error's
    /// status, `502` for upstreams' errors)
    fn fail_to_proxy(
        &self,
        session: &mut Session,
        e: &Error,
        _ctx: &mut Self::CTX,
    ) -> impl Future<Output = FailToProxy> + Send {
        async move {
            let code = default_error_status(e);
            if code > 0 && session.response_written().is_none() {
                if let Ok(header) = ResponseHeader::build(code, None) {
                    let _ = session.write_response_header(Box::new(header), true).await;
                }
            }
            FailToProxy {
                error_code: code,
                can_reuse_downstream: false,
            }
        }
    }

    /// Once the request is over: answered, or failed with `e`
    fn logging(
        &self,
        _session: &mut Session,
        _e: Option<&Error>,
        _ctx: &mut Self::CTX,
    ) -> impl Future<Output = ()> + Send {
        async {}
    }

    /// The key of the request in the cache
    ///
    /// # Errors
    ///
    /// When the request can't be cached.
    fn cache_key_callback(&self, session: &Session, _ctx: &mut Self::CTX) -> Result<CacheKey> {
        let request = session.req_header();
        let host = request
            .headers
            .get(http::header::HOST)
            .and_then(|host| host.to_str().ok())
            .or_else(|| request.uri.host())
            .unwrap_or_default();
        Ok(CacheKey::new(
            format!("{host}{}", request.uri),
            String::new(),
        ))
    }

    /// The response isn't in the cache: it's fetched from the upstream
    fn cache_miss(&self, session: &mut Session, _ctx: &mut Self::CTX) {
        session.cache.cache_miss();
    }

    /// A response found in the cache: whether to serve it (`None`), or to treat it as expired
    fn cache_hit_filter(
        &self,
        _session: &mut Session,
        _meta: &CacheMeta,
        _hit_handler: &mut HitHandler,
        _is_fresh: bool,
        _ctx: &mut Self::CTX,
    ) -> impl Future<Output = Result<Option<ForcedFreshness>>> + Send {
        async { Ok(None) }
    }

    /// Whether the upstream's response is cached, and until when
    ///
    /// # Errors
    ///
    /// An error leaves the response uncached.
    fn response_cache_filter(
        &self,
        _session: &Session,
        _resp: &ResponseHeader,
        _ctx: &mut Self::CTX,
    ) -> Result<RespCacheable> {
        Ok(RespCacheable::Uncacheable(NoCacheReason::Custom("default")))
    }
}

/// The status of a request that couldn't be proxied, as Pingora's (`0`: the client is gone)
pub fn default_error_status(e: &Error) -> u16 {
    match (e.etype(), e.esource()) {
        (ErrorType::HTTPStatus(code), _) => *code,
        (_, ErrorSource::Upstream) => 502,
        (
            ErrorType::WriteError | ErrorType::ReadError | ErrorType::ConnectionClosed,
            ErrorSource::Downstream,
        ) => 0,
        (_, ErrorSource::Downstream) => 400,
        (_, ErrorSource::Internal | ErrorSource::Unset) => 500,
    }
}

/// Proxies the requests of an HTTP service with the phases of `inner`
pub struct HttpProxy<SV> {
    pub inner: SV,
    connector: Arc<Connector>,
    options: Option<HttpServerOptions>,
}

impl<SV: ProxyHttp> HttpProxy<SV> {
    pub fn new(inner: SV, _conf: Arc<ServerConf>) -> Self {
        Self {
            inner,
            connector: Connector::shared(),
            options: None,
        }
    }

    pub fn server_options_mut(&mut self) -> &mut HttpServerOptions {
        self.options.get_or_insert_with(HttpServerOptions::default)
    }
}

/// A proxy of `inner`'s phases, sharing the upstream connections of every proxy
pub fn http_proxy<SV: ProxyHttp>(conf: &Arc<ServerConf>, inner: SV) -> HttpProxy<SV> {
    HttpProxy::new(inner, conf.clone())
}

impl<SV: ProxyHttp> HttpServerApp for HttpProxy<SV> {
    async fn process_new_http(self: &Arc<Self>, session: ServerSession, _shutdown: &ShutdownWatch) {
        engine::process(self, session).await;
    }

    fn server_options(&self) -> Option<&HttpServerOptions> {
        self.options.as_ref()
    }
}

#[cfg(test)]
mod tests;
