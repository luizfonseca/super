//! Timeouts toward clients (`server.timeouts`), for every HTTP and HTTPS connection, and the
//! defaults toward upstreams

use std::{sync::Arc, time::Duration};

use once_cell::sync::OnceCell;
use super_core::{
    apps::{HttpServerApp, HttpServerOptions},
    protocols::http::ServerSession,
    server::ShutdownWatch,
};

use crate::config::ClientTimeouts;

static TIMEOUTS: OnceCell<ClientTimeouts> = OnceCell::new();

/// Sets the timeouts of the HTTP and HTTPS apps built from now on
pub fn set_client_timeouts(timeouts: ClientTimeouts) {
    let _ = TIMEOUTS.set(timeouts);
}

/// `server.timeouts`
pub fn client_timeouts() -> ClientTimeouts {
    TIMEOUTS.get().copied().unwrap_or_default()
}

/// The timeouts toward upstreams of `server.timeouts`
pub fn upstream_timeouts() -> crate::config::host::UpstreamTimeouts {
    TIMEOUTS
        .get()
        .map(|timeouts| timeouts.upstream)
        .unwrap_or_default()
}

/// An HTTP app whose clients get the configured timeouts: idle connections are closed (between
/// requests, HTTP/2 included), and so are those of clients too slow to send or read
pub struct WithTimeouts<A> {
    app: Arc<A>,
    timeouts: ClientTimeouts,
    options: HttpServerOptions,
}

impl<A: HttpServerApp> WithTimeouts<A> {
    pub fn new(app: A) -> Self {
        let timeouts = TIMEOUTS.get().copied().unwrap_or_default();
        let mut options = app.server_options().cloned().unwrap_or_default();
        options.idle_timeout = Some(Duration::from_secs(timeouts.idle_secs));
        options.header_read_timeout = Some(Duration::from_secs(timeouts.read_secs));
        Self {
            app: Arc::new(app),
            timeouts,
            options,
        }
    }
}

impl<A: HttpServerApp + Send + Sync + 'static> HttpServerApp for WithTimeouts<A> {
    async fn process_new_http(
        self: &Arc<Self>,
        mut session: ServerSession,
        shutdown: &ShutdownWatch,
    ) {
        // Waiting for the next request on a kept-alive connection, unless it's closing
        if session.get_keepalive().is_some() {
            session.set_keepalive(Some(self.timeouts.idle_secs));
        }
        session.set_read_timeout(Some(Duration::from_secs(self.timeouts.read_secs)));
        session.set_write_timeout(Some(Duration::from_secs(self.timeouts.write_secs)));
        self.app.process_new_http(session, shutdown).await;
    }

    fn server_options(&self) -> Option<&HttpServerOptions> {
        Some(&self.options)
    }

    async fn http_cleanup(&self) {
        self.app.http_cleanup().await;
    }
}
