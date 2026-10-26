//! A connection's HTTP requests, served by hyper: each one is a [`ServerSession`] handed to the
//! app. Its response is sent once written; when its body is streamed, the rest of the handling
//! runs as hyper reads the body, in the connection's task.

use std::{
    future::{poll_fn, Future},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    task::Poll,
    time::{Duration, Instant},
};

use hyper::{body::Incoming, service::service_fn};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tokio::sync::oneshot;

use super::{HttpServerApp, HttpServerOptions};
use crate::{
    protocols::{
        http::{body::Driver, Connection, ResponseBody, ServerSession},
        Stream, ALPN,
    },
    server::ShutdownWatch,
};

/// A request that got no response: hyper closes its connection
#[derive(Debug)]
struct Unanswered;

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the request got no response")
    }
}

impl std::error::Error for Unanswered {}

/// Serves the requests of `stream` until it closes, or until shutting down
pub async fn serve<A: HttpServerApp>(app: &Arc<A>, stream: Stream, shutdown: &ShutdownWatch) {
    let http2 = matches!(stream.selected_alpn_proto(), Some(ALPN::H2));
    let connection = Arc::new(Connection {
        digest: stream.digest(),
        http2,
    });
    let options = app.server_options().cloned().unwrap_or_default();
    let activity = Arc::new(Activity::new());
    let service = {
        let (app, shutdown, activity) = (app.clone(), shutdown.clone(), activity.clone());
        service_fn(move |request| {
            let active = activity.start();
            handle(
                app.clone(),
                connection.clone(),
                shutdown.clone(),
                request,
                active,
            )
        })
    };
    let io = TokioIo::new(stream);
    let mut shutdown = shutdown.clone();
    if http2 {
        let mut builder = hyper::server::conn::http2::Builder::new(TokioExecutor::new());
        builder
            .timer(TokioTimer::new())
            .max_concurrent_streams(options.max_h2_streams);
        let connection = builder.serve_connection(io, service);
        drive(connection, &activity, &options, &mut shutdown, true).await;
        return;
    }
    let mut builder = hyper::server::conn::http1::Builder::new();
    // Header names go on as clients and upstreams wrote them; Super's own are Title-Case
    builder
        .timer(TokioTimer::new())
        .keep_alive(true)
        .preserve_header_case(true)
        .title_case_headers(true);
    if let Some(timeout) = options.header_read_timeout {
        builder.header_read_timeout(timeout);
    }
    let connection = builder.serve_connection(io, service).with_upgrades();
    drive(connection, &activity, &options, &mut shutdown, false).await;
}

/// A hyper connection, which can stop taking requests
trait Graceful: Future {
    fn graceful_shutdown(self: Pin<&mut Self>);
}

impl<I, S> Graceful for hyper::server::conn::http1::UpgradeableConnection<I, S>
where
    Self: Future,
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
    S: hyper::service::HttpService<Incoming>,
    S::ResBody: hyper::body::Body + 'static,
    <S::ResBody as hyper::body::Body>::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    fn graceful_shutdown(self: Pin<&mut Self>) {
        hyper::server::conn::http1::UpgradeableConnection::graceful_shutdown(self);
    }
}

impl<I, S, E> Graceful for hyper::server::conn::http2::Connection<I, S, E>
where
    Self: Future,
    I: hyper::rt::Read + hyper::rt::Write + Unpin,
    S: hyper::service::HttpService<Incoming>,
    S::ResBody: hyper::body::Body + 'static,
    <S::ResBody as hyper::body::Body>::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    E: hyper::rt::bounds::Http2ServerConnExec<S::Future, S::ResBody>,
{
    fn graceful_shutdown(self: Pin<&mut Self>) {
        hyper::server::conn::http2::Connection::graceful_shutdown(self);
    }
}

/// How long an HTTP/1.1 connection can stay idle once shutting down: a client reusing it
/// meanwhile gets its answer (and the connection is closed after it), instead of a connection
/// closed under its request
const IDLE_WHEN_STOPPING: Duration = Duration::from_secs(1);

/// Runs `connection` until it closes: once idle for too long, or once shutting down (HTTP/2
/// streams are told to go away right away; HTTP/1.1 connections answer one more request at most)
async fn drive<C, E>(
    connection: C,
    activity: &Activity,
    options: &HttpServerOptions,
    shutdown: &mut ShutdownWatch,
    http2: bool,
) where
    C: Graceful<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    let mut connection = std::pin::pin!(connection);
    let mut closed = false;
    if *shutdown.borrow() {
        closed = stop(connection.as_mut(), activity, http2);
    }
    // Without a server to say so, it never shuts down
    let mut watching = true;
    loop {
        let stopping = activity.is_stopping();
        let idle_timeout = match options.idle_timeout {
            _ if closed => None,
            Some(timeout) if stopping => Some(timeout.min(IDLE_WHEN_STOPPING)),
            None if stopping => Some(IDLE_WHEN_STOPPING),
            timeout => timeout,
        };
        let idle = async {
            match idle_timeout {
                Some(timeout) => activity.idle_for(timeout).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            served = connection.as_mut() => {
                if let Err(err) = served {
                    tracing::debug!("connection closed: {err}");
                }
                return;
            }
            changed = shutdown.changed(), if watching && !stopping => {
                watching = changed.is_ok() && !*shutdown.borrow();
                closed = !watching && changed.is_ok() && stop(connection.as_mut(), activity, http2);
            }
            () = idle => {
                closed = true;
                connection.as_mut().graceful_shutdown();
            }
        }
    }
}

/// The server is shutting down: the connection takes no more requests (HTTP/2), or one more
/// at most (HTTP/1.1). Returns whether it's closing already.
fn stop<C: Graceful>(connection: Pin<&mut C>, activity: &Activity, http2: bool) -> bool {
    activity.stop();
    if http2 {
        connection.graceful_shutdown();
    }
    http2
}

/// Hands a request to the app, and returns its response once written
async fn handle<A: HttpServerApp>(
    app: Arc<A>,
    connection: Arc<Connection>,
    shutdown: ShutdownWatch,
    request: hyper::Request<Incoming>,
    active: Active,
) -> Result<hyper::Response<ResponseBody>, Unanswered> {
    let (sender, mut response) = oneshot::channel();
    let mut session = ServerSession::new(request, connection, sender);
    // Shutting down: the connection closes after this response
    if active.0.is_stopping() {
        session.set_keepalive(None);
    }
    // Boxed once: once its response is sent, the rest of the handling goes on with the body
    let mut handling: Driver = Box::pin(async move {
        let _active = active;
        app.process_new_http(session, &shutdown).await;
    });
    let (answered, done) = poll_fn(|cx| {
        if let Poll::Ready(answered) = Pin::new(&mut response).poll(cx) {
            // What's left (e.g. logging) often finishes right away
            let done = handling.as_mut().poll(cx).is_ready();
            return Poll::Ready((answered.ok(), done));
        }
        handling.as_mut().poll(cx).map(|()| (None, true))
    })
    .await;
    let answered = answered.or_else(|| response.try_recv().ok());
    let Some(mut answered) = answered else {
        if !done {
            tokio::spawn(handling);
        }
        return Err(Unanswered);
    };
    if !done {
        answered.body_mut().drive(handling);
    }
    Ok(answered)
}

/// The requests in progress on a connection, and when the last one ended
struct Activity {
    started: Instant,
    active: AtomicUsize,
    /// Shutting down: responses close the connection
    stopping: AtomicBool,
    /// Milliseconds after `started`
    last: AtomicU64,
}

/// A request in progress, until dropped
struct Active(Arc<Activity>);

impl Drop for Active {
    fn drop(&mut self) {
        self.0.last.store(self.0.elapsed_ms(), Ordering::Relaxed);
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Activity {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            active: AtomicUsize::new(0),
            stopping: AtomicBool::new(false),
            last: AtomicU64::new(0),
        }
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn stop(&self) {
        self.stopping.store(true, Ordering::Relaxed);
    }

    fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Relaxed)
    }

    fn start(self: &Arc<Self>) -> Active {
        self.active.fetch_add(1, Ordering::Relaxed);
        Active(self.clone())
    }

    /// Returns once no request was in progress for `timeout`
    async fn idle_for(&self, timeout: Duration) {
        loop {
            let last = self.started + Duration::from_millis(self.last.load(Ordering::Relaxed));
            let idle = self.active.load(Ordering::Relaxed) == 0;
            if idle && last.elapsed() >= timeout {
                return;
            }
            let wake = if idle {
                last + timeout
            } else {
                Instant::now() + timeout
            };
            tokio::time::sleep_until(wake.into()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper_util::rt::TokioIo;

    use crate::{
        apps::{HttpServerApp, HttpServerOptions},
        http::ResponseHeader,
        protocols::{http::ServerSession, l4::stream::Stream},
        server::ShutdownWatch,
    };

    /// Answers `/full` at once, `/stream` in parts, and `/echo` with the request's body
    struct App;

    impl HttpServerApp for App {
        async fn process_new_http(self: &Arc<Self>, mut session: ServerSession, _: &ShutdownWatch) {
            let path = session.req_header().uri.path().to_string();
            let header = Box::new(ResponseHeader::build(200, None).unwrap());
            session.write_response_header(header).await.unwrap();
            match path.as_str() {
                "/full" => session
                    .write_response_body(Bytes::from("full"), true)
                    .await
                    .unwrap(),
                "/stream" => {
                    for part in ["a", "b", "c"] {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        session
                            .write_response_body(Bytes::from(part), false)
                            .await
                            .unwrap();
                    }
                    session.finish_body().await.unwrap();
                }
                "/close" => {
                    session.set_keepalive(None);
                    session.finish_body().await.unwrap();
                }
                _ => {
                    let mut body = vec![];
                    while let Some(part) = session.read_request_body().await.unwrap() {
                        body.extend_from_slice(&part);
                    }
                    session
                        .write_response_body(Bytes::from(body), true)
                        .await
                        .unwrap();
                }
            }
        }
    }

    /// An `App` closing connections idle for 100ms
    struct Idle(App, HttpServerOptions);

    impl HttpServerApp for Idle {
        async fn process_new_http(
            self: &Arc<Self>,
            session: ServerSession,
            shutdown: &ShutdownWatch,
        ) {
            Arc::new(App).process_new_http(session, shutdown).await;
        }

        fn server_options(&self) -> Option<&HttpServerOptions> {
            Some(&self.1)
        }
    }

    /// A client's connection to an `App`
    async fn connect() -> hyper::client::conn::http1::SendRequest<Full<Bytes>> {
        connect_to(Arc::new(App)).await
    }

    async fn connect_to<A: HttpServerApp>(
        app: Arc<A>,
    ) -> hyper::client::conn::http1::SendRequest<Full<Bytes>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let shutdown = tokio::sync::watch::channel(false).1;
            super::serve(&app, Box::new(Stream::from(socket)), &shutdown).await;
        });
        let socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
            .await
            .unwrap();
        tokio::spawn(connection);
        sender
    }

    async fn request(
        sender: &mut hyper::client::conn::http1::SendRequest<Full<Bytes>>,
        path: &str,
        body: &'static str,
    ) -> (http::response::Parts, Bytes) {
        sender.ready().await.unwrap();
        let request = http::Request::post(path)
            .header("host", "a.test")
            .body(Full::new(Bytes::from(body)))
            .unwrap();
        let (parts, body) = sender.send_request(request).await.unwrap().into_parts();
        (parts, body.collect().await.unwrap().to_bytes())
    }

    #[tokio::test]
    async fn test_responses_whole_streamed_and_echoed_on_one_connection() {
        let mut sender = connect().await;

        let (parts, body) = request(&mut sender, "/full", "").await;
        assert_eq!(parts.status, 200);
        assert_eq!(parts.headers["content-length"], "4");
        assert_eq!(body, "full");

        let (parts, body) = request(&mut sender, "/stream", "").await;
        assert_eq!(parts.headers["transfer-encoding"], "chunked");
        assert_eq!(body, "abc");

        let (_, body) = request(&mut sender, "/echo", "hello").await;
        assert_eq!(body, "hello");
    }

    #[tokio::test]
    async fn test_connections_closed_after_a_response() {
        let mut sender = connect().await;
        let (parts, _) = request(&mut sender, "/close", "").await;
        assert_eq!(parts.headers["connection"], "close");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(sender.ready().await.is_err());
    }

    #[tokio::test]
    async fn test_idle_connections_are_closed() {
        let options = HttpServerOptions {
            idle_timeout: Some(Duration::from_millis(100)),
            ..HttpServerOptions::default()
        };
        let mut sender = connect_to(Arc::new(Idle(App, options))).await;
        // Open while it serves requests
        let (_, body) = request(&mut sender, "/stream", "").await;
        assert_eq!(body, "abc");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(sender.ready().await.is_ok());

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(sender.ready().await.is_err());
    }

    #[tokio::test]
    async fn test_connections_answer_once_more_when_shutting_down() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stopping, shutdown) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let app = Arc::new(App);
            super::serve(&app, Box::new(Stream::from(socket)), &shutdown).await;
        });
        let socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
            .await
            .unwrap();
        tokio::spawn(connection);
        let (parts, _) = request(&mut sender, "/full", "").await;
        assert!(!parts.headers.contains_key("connection"));

        // A client reusing its connection meanwhile is answered, and told it's closing
        stopping.send_replace(true);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (parts, body) = request(&mut sender, "/full", "").await;
        assert_eq!(body, "full");
        assert_eq!(parts.headers["connection"], "close");
    }
}
