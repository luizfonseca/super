//! What listening services serve: connections ([`ServerApp`]), or the HTTP requests of each
//! one ([`HttpServerApp`], served by hyper)

use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use crate::{
    protocols::{http::ServerSession, Stream},
    server::ShutdownWatch,
};

mod http;
mod http3;
mod linger;

pub use http::serve;
pub(crate) use http::{MAX_HEADERS, MAX_HEADER_SIZE};
pub use http3::serve_quic;

/// Whether HTTP/1.1 header names go on as clients and upstreams wrote them (see
/// [`preserve_header_case`])
static PRESERVE_HEADER_CASE: AtomicBool = AtomicBool::new(false);

/// Keeps the case of HTTP/1.1 header names as clients and upstreams wrote them, instead of
/// Title-Case: for peers that compare names by case, which HTTP says they mustn't. Off unless
/// set, as it costs every request a map of the names' case. Set once, before serving.
pub fn preserve_header_case(on: bool) {
    PRESERVE_HEADER_CASE.store(on, Ordering::Relaxed);
}

pub(crate) fn header_case_preserved() -> bool {
    PRESERVE_HEADER_CASE.load(Ordering::Relaxed)
}

/// Serves the connections of a listening service
pub trait ServerApp: Send + Sync + 'static {
    /// Serves `stream`. Returns it when it can be served again (e.g. by another app).
    fn process_new(
        self: &Arc<Self>,
        stream: Stream,
        shutdown: &ShutdownWatch,
    ) -> impl Future<Output = Option<Stream>> + Send;

    /// Called when the service stops
    fn cleanup(&self) -> impl Future<Output = ()> + Send {
        async {}
    }
}

/// How the connections of an [`HttpServerApp`] are served
#[derive(Debug, Clone, Default)]
pub struct HttpServerOptions {
    /// How long a connection can wait without a request in progress
    pub idle_timeout: Option<Duration>,
    /// How long a client can take to send a request's headers (HTTP/1.1)
    pub header_read_timeout: Option<Duration>,
    /// HTTP/2 (and HTTP/3) streams a client can open at once
    pub max_h2_streams: Option<u32>,
}

/// Serves the HTTP requests of each connection, as sessions
pub trait HttpServerApp: Send + Sync + 'static {
    /// Answers the request of `session`
    fn process_new_http(
        self: &Arc<Self>,
        session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> impl Future<Output = ()> + Send;

    fn server_options(&self) -> Option<&HttpServerOptions> {
        None
    }

    /// Called when the service stops
    fn http_cleanup(&self) -> impl Future<Output = ()> + Send {
        async {}
    }
}

impl<T: HttpServerApp> ServerApp for T {
    async fn process_new(
        self: &Arc<Self>,
        stream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        serve(self, stream, shutdown).await;
        None
    }

    async fn cleanup(&self) {
        self.http_cleanup().await;
    }
}
