//! Requests proxied to real upstreams, through every phase

// Phases are async, whether or not they wait
#![allow(clippy::unused_async_trait_impl)]

use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, LazyLock, Mutex,
    },
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{http_proxy, HttpProxy, ProxyHttp, Session};
use crate::{
    apps::serve,
    cache::{lock::CacheLock, CacheKey, CacheMeta, MemCache, RespCacheable},
    http::{RequestHeader, ResponseHeader},
    protocols::l4::stream::Stream,
    server::configuration::ServerConf,
    upstreams::peer::HttpPeer,
    Error, ErrorType, Result,
};

/// An upstream: echoes the request (method, path, `x-seen` header and body), `/slow` takes
/// its time, `/ws` switches protocols and echoes bytes
async fn upstream() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let service = service_fn(answer);
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(socket), service)
                    .with_upgrades()
                    .await;
            });
        }
    });
    addr
}

async fn answer(
    mut request: hyper::Request<Incoming>,
) -> std::result::Result<hyper::Response<Full<Bytes>>, std::convert::Infallible> {
    if request.uri().path() == "/ws" {
        let upgrade = hyper::upgrade::on(&mut request);
        tokio::spawn(async move {
            let mut io = TokioIo::new(upgrade.await.unwrap());
            let mut buf = [0u8; 5];
            io.read_exact(&mut buf).await.unwrap();
            io.write_all(&buf).await.unwrap();
        });
        let response = hyper::Response::builder()
            .status(101)
            .header("connection", "upgrade")
            .header("upgrade", "test")
            .body(Full::new(Bytes::new()))
            .unwrap();
        return Ok(response);
    }
    if request.uri().path().contains("etag") {
        return Ok(versioned(&request).await);
    }
    if request.uri().path() == "/slow" {
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let seen = request
        .headers()
        .get("x-seen")
        .map(|value| value.to_str().unwrap().to_string())
        .unwrap_or_default();
    let line = format!("{} {} {seen} ", request.method(), request.uri());
    let body = request.into_body().collect().await.unwrap().to_bytes();
    let mut text = line.into_bytes();
    text.extend_from_slice(&body);
    let status = if body.as_ref() == b"fail" { 502 } else { 200 };
    let response = hyper::Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(Full::new(Bytes::from(text)))
        .unwrap();
    Ok(response)
}

/// Bodies answered by `versioned`, and `304`s
static FULL_ANSWERS: AtomicUsize = AtomicUsize::new(0);
static NOT_MODIFIED_ANSWERS: AtomicUsize = AtomicUsize::new(0);

/// A body with a validator (`ETag: "v1"`): `304` when the request has it (`slow`: after 300ms)
async fn versioned(request: &hyper::Request<Incoming>) -> hyper::Response<Full<Bytes>> {
    if request.uri().path().contains("slow") {
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let current = request
        .headers()
        .get("if-none-match")
        .is_some_and(|tag| tag == "\"v1\"");
    let builder = hyper::Response::builder().header("etag", "\"v1\"");
    if current {
        NOT_MODIFIED_ANSWERS.fetch_add(1, Ordering::Relaxed);
        return builder.status(304).body(Full::new(Bytes::new())).unwrap();
    }
    FULL_ANSWERS.fetch_add(1, Ordering::Relaxed);
    builder
        .header("content-type", "text/plain")
        .body(Full::new(Bytes::from_static(b"0123456789")))
        .unwrap()
}

/// What a request went through
#[derive(Default)]
struct Ctx {
    peers: usize,
    reused: Vec<bool>,
    body_parts: usize,
    errors: Option<String>,
}

/// The phases of the tests: to the upstreams in turn, with a header added
struct Phases {
    upstreams: Vec<SocketAddr>,
    logged: Logged,
    retry_on_502: bool,
}

static STORAGE: LazyLock<MemCache> = LazyLock::new(MemCache::new);
static LOCK: LazyLock<CacheLock> = LazyLock::new(|| CacheLock::new(Duration::from_secs(1)));
static CACHED_LOOKUPS: AtomicUsize = AtomicUsize::new(0);

impl ProxyHttp for Phases {
    type CTX = Ctx;

    fn new_ctx(&self) -> Ctx {
        Ctx::default()
    }

    async fn request_filter(&self, session: &mut Session, _ctx: &mut Ctx) -> Result<bool> {
        let path = session.req_header().uri.path().to_string();
        if path == "/answered" {
            let mut header = ResponseHeader::build(204, None)?;
            header.insert_header("x-answered", "yes")?;
            session
                .write_response_header(Box::new(header), true)
                .await?;
            return Ok(true);
        }
        if path.starts_with("/cached") {
            session.cache.enable(&*STORAGE, Some(&*LOCK));
        }
        if path == "/compressed" {
            let compression = session.downstream_modules_ctx.compression();
            compression.adjust_algorithm_level(crate::modules::compression::Algorithm::Gzip, 6);
            compression.request_filter(session.downstream_session.req_header());
        }
        Ok(false)
    }

    async fn upstream_peer(&self, _session: &mut Session, ctx: &mut Ctx) -> Result<Box<HttpPeer>> {
        let addr = self.upstreams[ctx.peers % self.upstreams.len()];
        ctx.peers += 1;
        let mut peer = HttpPeer::new(addr, false, String::new());
        peer.options.connection_timeout = Some(Duration::from_secs(1));
        peer.options.read_timeout = Some(Duration::from_secs(5));
        Ok(Box::new(peer))
    }

    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
        request: &mut RequestHeader,
        _ctx: &mut Ctx,
    ) -> Result<()> {
        request.insert_header("x-seen", "yes")
    }

    async fn request_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        _end: bool,
        ctx: &mut Ctx,
    ) -> Result<()> {
        if body.is_some() {
            ctx.body_parts += 1;
        }
        Ok(())
    }

    async fn upstream_response_filter(
        &self,
        _session: &mut Session,
        response: &mut ResponseHeader,
        ctx: &mut Ctx,
    ) -> Result<()> {
        if self.retry_on_502 && response.status == 502 && ctx.peers < 2 {
            let mut e = Error::explain(ErrorType::HTTPStatus(502), "tried again");
            e.set_retry(true);
            return Err(e);
        }
        Ok(())
    }

    async fn response_filter(
        &self,
        _session: &mut Session,
        response: &mut ResponseHeader,
        _ctx: &mut Ctx,
    ) -> Result<()> {
        response.insert_header("x-proxied", "super")
    }

    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        _ctx: &mut Ctx,
        mut e: Box<Error>,
    ) -> Box<Error> {
        e.set_retry(true);
        e
    }

    async fn connected_to_upstream(
        &self,
        _session: &mut Session,
        reused: bool,
        _peer: &HttpPeer,
        _fd: std::os::fd::RawFd,
        _digest: Option<&crate::protocols::Digest>,
        ctx: &mut Ctx,
    ) -> Result<()> {
        ctx.reused.push(reused);
        Ok(())
    }

    async fn logging(&self, session: &mut Session, e: Option<&Error>, ctx: &mut Ctx) {
        ctx.errors = e.map(ToString::to_string);
        let status = session
            .response_written()
            .map_or(0, |written| written.status.as_u16());
        let mut logged = self.logged.lock().unwrap();
        logged.push((status, ctx.errors.take(), std::mem::take(&mut ctx.reused)));
    }

    fn cache_key_callback(&self, session: &Session, _ctx: &mut Ctx) -> Result<CacheKey> {
        Ok(CacheKey::new(session.req_header().uri.path(), "test"))
    }

    async fn cache_hit_filter(
        &self,
        session: &mut Session,
        _meta: &CacheMeta,
        _hit: &mut crate::cache::HitHandler,
        _fresh: bool,
        _ctx: &mut Ctx,
    ) -> Result<Option<crate::cache::ForcedFreshness>> {
        // Of `test_responses_from_the_cache` only (tests run at once)
        if session.req_header().uri.path() == "/cached-a" {
            CACHED_LOOKUPS.fetch_add(1, Ordering::Relaxed);
        }
        Ok(None)
    }

    fn response_cache_filter(
        &self,
        session: &Session,
        response: &ResponseHeader,
        _ctx: &mut Ctx,
    ) -> Result<RespCacheable> {
        let now = SystemTime::now();
        // `stale` paths are stale right away, and served stale for 10s while revalidated
        let stale = session.req_header().uri.path().contains("stale");
        let (fresh, while_revalidating) = if stale { (0, 10) } else { (60, 0) };
        Ok(RespCacheable::Cacheable(CacheMeta::new(
            now + Duration::from_secs(fresh),
            now,
            while_revalidating,
            0,
            response.clone(),
        )))
    }
}

type Logged = Arc<Mutex<Vec<(u16, Option<String>, Vec<bool>)>>>;

/// A proxy to `upstreams`, and a client's connection to it
async fn proxy_to(
    upstreams: Vec<SocketAddr>,
    retry_on_502: bool,
) -> (hyper::client::conn::http1::SendRequest<Full<Bytes>>, Logged) {
    let logged = Logged::default();
    let phases = Phases {
        upstreams,
        logged: logged.clone(),
        retry_on_502,
    };
    let proxy: Arc<HttpProxy<Phases>> =
        Arc::new(http_proxy(&Arc::new(ServerConf::default()), phases));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let proxy = proxy.clone();
            tokio::spawn(async move {
                let shutdown = tokio::sync::watch::channel(false).1;
                serve(&proxy, Box::new(Stream::from(socket)), &shutdown).await;
            });
        }
    });
    let socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (sender, connection) = hyper::client::conn::http1::Builder::new()
        .handshake(TokioIo::new(socket))
        .await
        .unwrap();
    tokio::spawn(connection.with_upgrades());
    (sender, logged)
}

async fn request(
    sender: &mut hyper::client::conn::http1::SendRequest<Full<Bytes>>,
    method: &str,
    path: &str,
    body: &'static str,
) -> (http::response::Parts, Bytes) {
    sender.ready().await.unwrap();
    let request = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("host", "a.test")
        .header("accept-encoding", "gzip")
        .body(Full::new(Bytes::from(body)))
        .unwrap();
    let (parts, body) = sender.send_request(request).await.unwrap().into_parts();
    (parts, body.collect().await.unwrap().to_bytes())
}

/// The addresses of a listener that's gone: connections to it are refused
fn refusing() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

#[tokio::test]
async fn test_requests_go_through_every_phase() {
    let (mut client, logged) = proxy_to(vec![upstream().await], false).await;

    let (parts, body) = request(&mut client, "GET", "/a?b=c", "").await;
    assert_eq!(parts.status, 200);
    assert_eq!(parts.headers["x-proxied"], "super");
    assert_eq!(body, "GET /a?b=c yes ");

    let (_, body) = request(&mut client, "POST", "/echo", "hello").await;
    assert_eq!(body, "POST /echo yes hello");

    let (parts, body) = request(&mut client, "GET", "/answered", "").await;
    assert_eq!(parts.status, 204);
    assert_eq!(parts.headers["x-answered"], "yes");
    assert!(body.is_empty());

    tokio::time::sleep(Duration::from_millis(50)).await;
    let logged = logged.lock().unwrap();
    assert_eq!(logged.len(), 3);
    assert_eq!(logged[0], (200, None, vec![false]));
    // The connection to the upstream was kept for the next request
    assert_eq!(logged[1], (200, None, vec![true]));
    assert_eq!(logged[2], (204, None, vec![]));
}

#[tokio::test]
async fn test_requests_are_tried_again() {
    // Refused, then answered by the next upstream
    let (mut client, logged) = proxy_to(vec![refusing(), upstream().await], false).await;
    let (parts, body) = request(&mut client, "POST", "/again", "body").await;
    assert_eq!(parts.status, 200);
    assert_eq!(body, "POST /again yes body");

    // A 502 tried again (`upstream_response_filter`), with its body sent again
    let (mut client, _) = proxy_to(vec![upstream().await], true).await;
    let (parts, _) = request(&mut client, "POST", "/again", "fail").await;
    assert_eq!(parts.status, 502);

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(logged.lock().unwrap()[0].0, 200);
}

#[tokio::test]
async fn test_upstreams_that_fail() {
    let (mut client, logged) = proxy_to(vec![refusing()], false).await;
    let (parts, _) = request(&mut client, "GET", "/", "").await;
    assert_eq!(parts.status, 502);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let logged = logged.lock().unwrap();
    assert!(
        logged[0].1.as_ref().unwrap().contains("Connect"),
        "{:?}",
        logged[0]
    );
}

#[tokio::test]
async fn test_responses_from_the_cache() {
    let (mut client, _) = proxy_to(vec![upstream().await], false).await;
    let before = CACHED_LOOKUPS.load(Ordering::Relaxed);
    let (_, first) = request(&mut client, "GET", "/cached-a", "").await;
    let (parts, second) = request(&mut client, "GET", "/cached-a", "").await;
    assert_eq!(first, second);
    assert_eq!(parts.headers["x-proxied"], "super");
    assert_eq!(CACHED_LOOKUPS.load(Ordering::Relaxed), before + 1);
}

#[tokio::test]
async fn test_responses_compressed() {
    let (mut client, _) = proxy_to(vec![upstream().await], false).await;
    let (parts, body) = request(
        &mut client,
        "POST",
        "/compressed",
        "a long enough body, compressed",
    )
    .await;
    assert_eq!(parts.headers["content-encoding"], "gzip");
    let mut text = String::new();
    std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&body[..]), &mut text).unwrap();
    assert_eq!(text, "POST /compressed yes a long enough body, compressed");
}

#[tokio::test]
async fn test_upgraded_connections() {
    let (mut client, _) = proxy_to(vec![upstream().await], false).await;
    client.ready().await.unwrap();
    let request = hyper::Request::get("/ws")
        .header("host", "a.test")
        .header("connection", "upgrade")
        .header("upgrade", "test")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = client.send_request(request).await.unwrap();
    assert_eq!(response.status(), 101);
    let mut io = TokioIo::new(hyper::upgrade::on(response).await.unwrap());
    io.write_all(b"hello").await.unwrap();
    let mut echoed = [0u8; 5];
    io.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"hello");
}

/// Waits 50ms for its upstream
struct Impatient(SocketAddr);

impl ProxyHttp for Impatient {
    type CTX = ();

    fn new_ctx(&self) {}

    async fn upstream_peer(&self, _: &mut Session, (): &mut ()) -> Result<Box<HttpPeer>> {
        let mut peer = HttpPeer::new(self.0, false, String::new());
        peer.options.read_timeout = Some(Duration::from_millis(50));
        Ok(Box::new(peer))
    }
}

#[tokio::test]
async fn test_slow_upstreams_time_out() {
    let addr = upstream().await;
    let proxy = Arc::new(http_proxy(
        &Arc::new(ServerConf::default()),
        Impatient(addr),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let shutdown = tokio::sync::watch::channel(false).1;
        serve(&proxy, Box::new(Stream::from(socket)), &shutdown).await;
    });
    let socket = tokio::net::TcpStream::connect(proxy_addr).await.unwrap();
    let (mut client, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
        .await
        .unwrap();
    tokio::spawn(connection);
    let (parts, _) = request(&mut client, "GET", "/slow", "").await;
    assert_eq!(parts.status, 502);
}

/// Allocations of a request through the proxy, beyond those of the client and the upstream (and
/// of these tests' phases): hyper's parsing and dispatch, and the request sent to the upstream.
/// Kept from growing.
#[tokio::test]
async fn test_allocations_of_a_proxied_request() {
    /// What a request costs today, at most
    const PROXY: usize = 38;

    let addr = upstream().await;
    let socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut direct, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
        .await
        .unwrap();
    tokio::spawn(connection);
    let (mut proxied, _) = proxy_to(vec![addr], false).await;
    // Warm: connections and buffers are there
    request(&mut direct, "GET", "/a", "").await;
    request(&mut proxied, "GET", "/a", "").await;

    let (without, _) = count(request(&mut direct, "GET", "/a", "")).await;
    let (with, _) = count(request(&mut proxied, "GET", "/a", "")).await;
    let added = with.saturating_sub(without);
    assert!(
        added <= PROXY,
        "a proxied request allocates {added} times (at most {PROXY})"
    );
}

/// The allocations made on this thread while `future` runs (and what's spawned on it)
async fn count<F: std::future::Future>(future: F) -> (usize, F::Output) {
    crate::allocations::start();
    let output = future.await;
    (crate::allocations::stop(), output)
}

/// An upstream that answers each request before reading its body, then reads and drops the body
/// and keeps the connection (as nginx's `return`)
async fn early_upstream() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            tokio::spawn(answer_early(socket));
        }
    });
    addr
}

async fn answer_early(socket: tokio::net::TcpStream) {
    let mut socket = tokio::io::BufReader::new(socket);
    while let Some(length) = read_head(&mut socket).await {
        let response = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok";
        socket.get_mut().write_all(response).await.unwrap();
        tokio::io::copy(&mut (&mut socket).take(length), &mut tokio::io::sink())
            .await
            .unwrap();
    }
}

/// The length of the body of the next request, once its head is read (`None`: no request)
async fn read_head(socket: &mut tokio::io::BufReader<tokio::net::TcpStream>) -> Option<u64> {
    use tokio::io::AsyncBufReadExt;
    let mut length = 0;
    let mut line = String::new();
    while socket.read_line(&mut line).await.unwrap_or(0) > 2 {
        let lowercase = line.to_ascii_lowercase();
        if let Some(value) = lowercase.strip_prefix("content-length:") {
            length = value.trim().parse::<u64>().unwrap();
        }
        line.clear();
    }
    (!line.is_empty()).then_some(length)
}

/// The upstream answers before it reads the body: the client gets its answer
#[tokio::test]
async fn test_uploads_answered_early() {
    let addr = early_upstream().await;
    let body: &'static str = Box::leak("x".repeat(4 << 20).into_boxed_str());
    for _ in 0..3 {
        // Clients may close once answered, before their body was sent: a connection each
        let (mut client, logged) = proxy_to(vec![addr], false).await;
        let (parts, answer) = request(&mut client, "POST", "/early", body).await;
        assert_eq!(parts.status, 200, "{:?}", logged.lock().unwrap());
        assert_eq!(answer, "ok");
    }
}

/// An upstream whose body stops coming after its first bytes
async fn stalling_upstream() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut head = [0u8; 1024];
                let _ = socket.read(&mut head).await;
                let response = b"HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\nab";
                let _ = socket.write_all(response).await;
                tokio::time::sleep(Duration::from_secs(30)).await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn test_bodies_that_stop_coming_time_out() {
    let proxy = Arc::new(http_proxy(
        &Arc::new(ServerConf::default()),
        Impatient(stalling_upstream().await),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let shutdown = tokio::sync::watch::channel(false).1;
        serve(&proxy, Box::new(Stream::from(socket)), &shutdown).await;
    });
    let socket = tokio::net::TcpStream::connect(proxy_addr).await.unwrap();
    let (mut client, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
        .await
        .unwrap();
    tokio::spawn(connection);
    let request = hyper::Request::get("/")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let started = std::time::Instant::now();
    let response = client.send_request(request).await.unwrap();
    // Cut short within one to two timeouts (50ms each)
    assert!(response.into_body().collect().await.is_err());
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

async fn get(
    sender: &mut hyper::client::conn::http1::SendRequest<Full<Bytes>>,
    path: &str,
    headers: &[(&str, &str)],
) -> (http::response::Parts, Bytes) {
    sender.ready().await.unwrap();
    let mut request = hyper::Request::get(path).header("host", "a.test");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = sender.send_request(request.body(Full::new(Bytes::new())).unwrap());
    let (parts, body) = response.await.unwrap().into_parts();
    (parts, body.collect().await.unwrap().to_bytes())
}

#[tokio::test]
async fn test_conditional_and_range_requests_from_the_cache() {
    let (mut client, _) = proxy_to(vec![upstream().await], false).await;
    let (_, body) = get(&mut client, "/cached-etag-a", &[]).await;
    assert_eq!(body, "0123456789");

    // The client's copy is current
    let (parts, body) = get(
        &mut client,
        "/cached-etag-a",
        &[("if-none-match", "\"v1\"")],
    )
    .await;
    assert_eq!(parts.status, 304);
    assert!(body.is_empty());
    // A part of the body
    let (parts, body) = get(&mut client, "/cached-etag-a", &[("range", "bytes=2-4")]).await;
    assert_eq!(parts.status, 206);
    assert_eq!(parts.headers["content-range"], "bytes 2-4/10");
    assert_eq!(body, "234");
    let (parts, _) = get(&mut client, "/cached-etag-a", &[("range", "bytes=50-")]).await;
    assert_eq!(parts.status, 416);
    // And the whole body still
    let (parts, body) = get(&mut client, "/cached-etag-a", &[]).await;
    assert_eq!(
        (parts.status.as_u16(), body.as_ref()),
        (200, &b"0123456789"[..])
    );
}

#[tokio::test]
async fn test_stale_responses_are_revalidated_and_served_meanwhile() {
    let (mut first, _) = proxy_to(vec![upstream().await], false).await;
    get(&mut first, "/cached-etag-stale-slow", &[]).await;
    let not_modified = NOT_MODIFIED_ANSWERS.load(Ordering::Relaxed);

    // Stale: one request revalidates it (300ms), another is served the stale response at once
    let (mut second, _) = proxy_to(vec![upstream().await], false).await;
    let refreshing = tokio::spawn(async move {
        let started = std::time::Instant::now();
        let (parts, body) = get(&mut first, "/cached-etag-stale-slow", &[]).await;
        (parts.status, body, started.elapsed())
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let started = std::time::Instant::now();
    let (parts, body) = get(&mut second, "/cached-etag-stale-slow", &[]).await;
    assert_eq!(
        (parts.status.as_u16(), body.as_ref()),
        (200, &b"0123456789"[..])
    );
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "{:?}",
        started.elapsed()
    );

    let (status, body, took) = refreshing.await.unwrap();
    assert_eq!((status.as_u16(), body.as_ref()), (200, &b"0123456789"[..]));
    assert!(took >= Duration::from_millis(300), "{took:?}");
    // Revalidated: a `304`, not the body again
    assert_eq!(
        NOT_MODIFIED_ANSWERS.load(Ordering::Relaxed),
        not_modified + 1
    );
}
