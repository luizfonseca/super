//! Every client protocol (HTTP/1.1, HTTP/2, HTTP/3), to every upstream protocol (HTTP/1.1,
//! HTTP/1.1 over TLS, HTTP/2 over TLS), through the phases that change the request (its `Host`,
//! its path, its headers), with and without a body: what each upstream gets

use std::{net::SocketAddr, pin::Pin, sync::Arc};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, service::service_fn};
use hyper_util::rt::{TokioExecutor, TokioIo};
use openssl::ssl::{AlpnError, NameType, Ssl, SslAcceptor, SslMethod};

use super::{h3_client, H3Client, Http2};
use crate::{
    apps::serve,
    http::RequestHeader,
    protocols::{l4::stream::Stream, ALPN},
    proxy::{http_proxy, HttpProxy, ProxyHttp, Session},
    server::configuration::ServerConf,
    upstreams::peer::HttpPeer,
    Result,
};

/// What a client sends as its host, and the name of the external service the `/host` phases
/// send requests to (its `Host`, and its TLS server name)
const CLIENT: &str = "client.test";
const SERVICE: &str = "service.test";

#[derive(Clone, Copy, Debug)]
enum Upstream {
    Http1,
    Http1Tls,
    Http2Tls,
}

#[derive(Clone, Copy, Debug)]
enum Client {
    Http1,
    Http2,
    Http3,
}

/// The requests of the matrix, by path: what the phases change
const PATHS: [&str; 5] = [
    "/plain?a=1",
    "/host",
    "/rewrite",
    "/host/rewrite",
    "/headers",
];

/// What the upstream gets for a request to `path`, `body` with it, from `upstream`
fn expected(upstream: Upstream, path: &str, body: &str) -> String {
    let version = match upstream {
        Upstream::Http1 | Upstream::Http1Tls => "HTTP/1.1",
        Upstream::Http2Tls => "HTTP/2.0",
    };
    let host = if path.starts_with("/host") {
        SERVICE
    } else {
        CLIENT
    };
    let sni = match upstream {
        Upstream::Http1 => "-",
        Upstream::Http1Tls | Upstream::Http2Tls => host,
    };
    let target = if path.ends_with("rewrite") {
        "/rewritten?q=1"
    } else {
        path
    };
    let headers = path == "/headers";
    format!(
        "{version} {host} {target} sni={sni} added={headers} removed={headers} body={}",
        body.len()
    )
}

/// The phases: each path's change to the request sent upstream
struct Matrix {
    upstream: SocketAddr,
    kind: Upstream,
}

impl ProxyHttp for Matrix {
    type CTX = ();

    fn new_ctx(&self) {}

    async fn upstream_peer(&self, session: &mut Session, (): &mut ()) -> Result<Box<HttpPeer>> {
        let path = session.req_header().uri.path();
        let sni = if path.starts_with("/host") {
            SERVICE
        } else {
            CLIENT
        };
        let tls = !matches!(self.kind, Upstream::Http1);
        let mut peer = HttpPeer::new(self.upstream, tls, sni.to_string());
        peer.options.verify_cert = false;
        peer.options.verify_hostname = false;
        peer.options.alpn = match self.kind {
            Upstream::Http2Tls => ALPN::H2,
            Upstream::Http1 | Upstream::Http1Tls => ALPN::H1,
        };
        Ok(Box::new(peer))
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        request: &mut RequestHeader,
        (): &mut (),
    ) -> Result<()> {
        let path = session.req_header().uri.path();
        // An external service's `Host`, as a location's upstream sets it
        if path.starts_with("/host") {
            request.insert_header(http::header::HOST, SERVICE)?;
        }
        // A location's rewrite
        if path.ends_with("rewrite") {
            request.set_uri(http::Uri::from_static("/rewritten?q=1"));
        }
        if path == "/headers" {
            request.insert_header("x-added", "1")?;
            request.remove_header("x-removed");
        }
        Ok(())
    }
}

/// An upstream of `kind`: echoes what it got as `expected` writes it. Over TLS, it answers
/// `421` when the request's host isn't the server name it was asked for, as CDNs do.
async fn upstream(kind: Upstream) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = match kind {
        Upstream::Http1 => None,
        Upstream::Http1Tls => Some(acceptor(b"\x08http/1.1")),
        Upstream::Http2Tls => Some(acceptor(b"\x02h2")),
    };
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Some(acceptor) = acceptor else {
                    let service = service_fn(|request| echo(request, None));
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(socket), service)
                        .await;
                    return;
                };
                let ssl = Ssl::new(acceptor.context()).unwrap();
                let mut stream = tokio_openssl::SslStream::new(ssl, socket).unwrap();
                if Pin::new(&mut stream).accept().await.is_err() {
                    return;
                }
                let sni = stream
                    .ssl()
                    .servername(NameType::HOST_NAME)
                    .unwrap_or("-")
                    .to_string();
                let http2 = stream.ssl().selected_alpn_protocol() == Some(b"h2");
                let service = service_fn(move |request| echo(request, Some(sni.clone())));
                if http2 {
                    let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                } else {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                }
            });
        }
    });
    addr
}

/// TLS with a certificate of its own, offering `protocols` (ALPN)
fn acceptor(protocols: &'static [u8]) -> Arc<SslAcceptor> {
    let (certificate, key) = super::self_signed("upstream.test");
    let mut builder = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).unwrap();
    builder.set_certificate(&certificate).unwrap();
    builder.set_private_key(&key).unwrap();
    builder.set_alpn_select_callback(move |_, offered| {
        openssl::ssl::select_next_proto(protocols, offered).ok_or(AlpnError::NOACK)
    });
    Arc::new(builder.build())
}

async fn echo(
    request: hyper::Request<Incoming>,
    sni: Option<String>,
) -> std::result::Result<hyper::Response<Full<Bytes>>, std::convert::Infallible> {
    // HTTP/2 has the host in the URI (`:authority`), HTTP/1.1 in `Host`
    let host = match request.uri().authority() {
        Some(authority) => authority.to_string(),
        None => header(&request, "host"),
    };
    let target = request
        .uri()
        .path_and_query()
        .map_or("/", http::uri::PathAndQuery::as_str)
        .to_string();
    let added = !header(&request, "x-added").is_empty();
    let removed = header(&request, "x-removed").is_empty();
    let version = request.version();
    let body = request.into_body().collect().await.unwrap().to_bytes();
    let seen = format!(
        "{version:?} {host} {target} sni={} added={added} removed={removed} body={}",
        sni.as_deref().unwrap_or("-"),
        body.len()
    );
    // A CDN in front of the service: a request for another host is refused
    let status = match &sni {
        Some(sni) if *sni != host => 421,
        _ => 200,
    };
    Ok(hyper::Response::builder()
        .status(status)
        .body(Full::new(Bytes::from(seen)))
        .unwrap())
}

fn header(request: &hyper::Request<Incoming>, name: &str) -> String {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// A proxy to `upstream`, listening for clients speaking `client`
async fn proxy(upstream: SocketAddr, kind: Upstream, client: Client) -> Sender {
    let proxy: Arc<HttpProxy<Matrix>> = Arc::new(http_proxy(
        &Arc::new(ServerConf::default()),
        Matrix { upstream, kind },
    ));
    if let Client::Http3 = client {
        return Sender::Http3(h3_client(proxy).await);
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let http2 = matches!(client, Client::Http2);
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let proxy = proxy.clone();
            let stream: crate::protocols::Stream = if http2 {
                Box::new(Http2(socket))
            } else {
                Box::new(Stream::from(socket))
            };
            tokio::spawn(async move {
                let shutdown = tokio::sync::watch::channel(false).1;
                serve(&proxy, stream, &shutdown).await;
            });
        }
    });
    let socket = TokioIo::new(tokio::net::TcpStream::connect(addr).await.unwrap());
    if http2 {
        let (sender, connection) = hyper::client::conn::http2::Builder::new(TokioExecutor::new())
            .handshake(socket)
            .await
            .unwrap();
        tokio::spawn(connection);
        return Sender::Http2(sender);
    }
    let (sender, connection) = hyper::client::conn::http1::handshake(socket).await.unwrap();
    tokio::spawn(connection);
    Sender::Http1(sender)
}

enum Sender {
    Http1(hyper::client::conn::http1::SendRequest<Full<Bytes>>),
    Http2(hyper::client::conn::http2::SendRequest<Full<Bytes>>),
    Http3(H3Client),
}

impl Sender {
    /// The status and body of the answer to `method` `path` with `body`, sent for `CLIENT`
    async fn send(&mut self, method: &str, path: &str, body: &'static str) -> (u16, Bytes) {
        let request = |uri: String| {
            hyper::Request::builder()
                .method(method)
                .uri(uri)
                .header("x-removed", "1")
        };
        let (parts, received) = match self {
            Self::Http1(sender) => {
                let request = request(path.to_string())
                    .header("host", CLIENT)
                    .body(Full::new(Bytes::from(body)))
                    .unwrap();
                sender.ready().await.unwrap();
                let (parts, body) = sender.send_request(request).await.unwrap().into_parts();
                (parts, body.collect().await.unwrap().to_bytes())
            }
            Self::Http2(sender) => {
                let request = request(format!("https://{CLIENT}{path}"))
                    .body(Full::new(Bytes::from(body)))
                    .unwrap();
                sender.ready().await.unwrap();
                let (parts, body) = sender.send_request(request).await.unwrap().into_parts();
                (parts, body.collect().await.unwrap().to_bytes())
            }
            Self::Http3(sender) => {
                let request = request(format!("https://{CLIENT}{path}")).body(()).unwrap();
                let mut stream = sender.send_request(request).await.unwrap();
                if !body.is_empty() {
                    stream.send_data(Bytes::from(body)).await.unwrap();
                }
                stream.finish().await.unwrap();
                let response = stream.recv_response().await.unwrap();
                let mut received = Vec::new();
                while let Some(mut data) = stream.recv_data().await.unwrap() {
                    use bytes::Buf;
                    received.extend_from_slice(&data.copy_to_bytes(data.remaining()));
                }
                (response.into_parts().0, Bytes::from(received))
            }
        };
        (parts.status.as_u16(), received)
    }
}

/// What `client` and the upstream of `kind` get wrong, request by request
async fn check(kind: Upstream, client: Client) -> Vec<String> {
    let mut sender = proxy(upstream(kind).await, kind, client).await;
    let requests = PATHS.into_iter().flat_map(|path| {
        [("GET", ""), ("POST", "hello")].map(|(method, body)| (path, method, body))
    });
    let mut failures = vec![];
    for (path, method, body) in requests {
        let (status, seen) = sender.send(method, path, body).await;
        let want = expected(kind, path, body);
        if status != 200 || seen != want {
            failures.push(format!(
                "{client:?} → {kind:?} {method} {path}: {status} {seen:?}, wanted 200 {want:?}"
            ));
        }
    }
    failures
}

#[tokio::test]
async fn test_every_client_to_every_upstream() {
    let combinations = [Upstream::Http1, Upstream::Http1Tls, Upstream::Http2Tls]
        .into_iter()
        .flat_map(|kind| {
            [Client::Http1, Client::Http2, Client::Http3].map(|client| (kind, client))
        });
    let mut failures = vec![];
    for (kind, client) in combinations {
        failures.extend(check(kind, client).await);
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
