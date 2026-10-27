//! An external HTTPS service, as a CDN serves one: a certificate for `service.test` signed by a
//! root of the tests' own (trusted by proksi through `SSL_CERT_FILE`), HTTP/2 or HTTP/1.1 as ALPN
//! chooses, and `421` for a request whose host isn't the server name it was asked for. It echoes
//! the request it got as JSON.

use std::{path::Path, pin::Pin, sync::Arc};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, service::service_fn};
use hyper_util::rt::{TokioExecutor, TokioIo};
use openssl::ssl::{AlpnError, NameType, Ssl, SslAcceptor, SslMethod};
use tokio::net::TcpListener;

use super::signed_certificate;

/// The name the service's certificate is for
pub const SERVICE: &str = "service.test";

pub struct Service {
    pub port: u16,
}

impl Service {
    /// Starts the service, its root written to `root` (a PEM file to trust)
    pub async fn start(root: &Path) -> Self {
        let authority = signed_certificate("service root", None, true);
        let (certificate, key) = signed_certificate(SERVICE, Some(&authority), false);
        std::fs::write(root, authority.0.to_pem().unwrap()).unwrap();

        let mut builder = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).unwrap();
        builder.set_certificate(&certificate).unwrap();
        builder.set_private_key(&key).unwrap();
        builder.set_alpn_select_callback(|_, offered| {
            openssl::ssl::select_next_proto(b"\x02h2\x08http/1.1", offered).ok_or(AlpnError::NOACK)
        });
        let acceptor = Arc::new(builder.build());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(serve(acceptor.clone(), socket));
            }
        });
        Self { port }
    }
}

async fn serve(acceptor: Arc<SslAcceptor>, socket: tokio::net::TcpStream) {
    let ssl = Ssl::new(acceptor.context()).unwrap();
    let mut stream = tokio_openssl::SslStream::new(ssl, socket).unwrap();
    if Pin::new(&mut stream).accept().await.is_err() {
        return;
    }
    let sni = stream
        .ssl()
        .servername(NameType::HOST_NAME)
        .unwrap_or_default()
        .to_string();
    let http2 = stream.ssl().selected_alpn_protocol() == Some(b"h2");
    let service = service_fn(move |request| echo(request, sni.clone()));
    let io = TokioIo::new(stream);
    if http2 {
        let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
            .serve_connection(io, service)
            .await;
        return;
    }
    let _ = hyper::server::conn::http1::Builder::new()
        .serve_connection(io, service)
        .await;
}

/// The request as JSON: its method, path (and query), host (HTTP/2's `:authority`, HTTP/1.1's
/// `Host`), version, the server name it came for, headers and body
async fn echo(
    request: hyper::Request<Incoming>,
    sni: String,
) -> Result<hyper::Response<Full<Bytes>>, std::convert::Infallible> {
    let host = match request.uri().authority() {
        Some(authority) => authority.to_string(),
        None => request
            .headers()
            .get("host")
            .and_then(|host| host.to_str().ok())
            .unwrap_or_default()
            .to_string(),
    };
    let headers: serde_json::Map<String, serde_json::Value> = request
        .headers()
        .iter()
        .map(|(name, value)| {
            let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
            (name.to_string(), value.into())
        })
        .collect();
    let seen = serde_json::json!({
        "method": request.method().as_str(),
        "path": request.uri().path_and_query().map_or("/", |path| path.as_str()),
        "host": host,
        "version": format!("{:?}", request.version()),
        "sni": sni,
        "headers": headers,
    });
    let body = request.into_body().collect().await.unwrap().to_bytes();
    let mut seen = seen;
    seen["body"] = String::from_utf8_lossy(&body).into_owned().into();
    // A CDN in front of the service: a request for another host is refused
    let status = if host == sni { 200 } else { 421 };
    Ok(hyper::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("x-powered-by", "service")
        .body(Full::new(Bytes::from(seen.to_string())))
        .unwrap())
}
