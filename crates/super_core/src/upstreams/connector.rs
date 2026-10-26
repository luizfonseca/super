//! Connections to upstreams, over TCP or TLS, with HTTP/1.1 or HTTP/2 (as negotiated), kept for
//! the next requests to the same upstream

use std::{
    os::fd::{AsRawFd, RawFd},
    sync::{Arc, LazyLock},
    time::Duration,
};

use hyper::client::conn::{http1, http2};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode};
use tokio::net::{TcpSocket, TcpStream};
use tokio_openssl::SslStream;

use super::{
    body::UpstreamBody,
    peer::{HttpPeer, Peer},
    pool::{H2Stream, Pool},
};
use crate::{
    protocols::{l4::stream::Stream as L4Stream, tls::TlsStream, SslDigest, Stream, ALPN, IO},
    Error, ErrorType, OrErr, Result,
};

/// How a request is sent on a connection
pub enum Sender {
    H1(http1::SendRequest<UpstreamBody>),
    H2(http2::SendRequest<UpstreamBody>, H2Stream),
}

/// A connection to an upstream, for one request
pub struct HttpSession {
    pub sender: Sender,
    /// What the pool keeps it by
    pub(crate) key: u64,
    /// The shard of the thread that opened it, where it's kept
    pub(crate) home: usize,
    pub fd: RawFd,
}

impl HttpSession {
    pub fn is_http2(&self) -> bool {
        matches!(self.sender, Sender::H2(..))
    }
}

/// Connects to upstreams, and keeps their connections
pub struct Connector {
    pool: Pool,
    tls: Option<SslConnector>,
}

impl Connector {
    pub fn new() -> Self {
        let tls = SslConnector::builder(SslMethod::tls_client())
            .map(openssl::ssl::SslConnectorBuilder::build)
            .map_err(|err| tracing::error!("can't set up TLS to upstreams: {err}"))
            .ok();
        Self {
            pool: Pool::new(),
            tls,
        }
    }

    /// The connector every proxy shares (with its connections, and the system's certificates)
    pub fn shared() -> Arc<Connector> {
        static SHARED: LazyLock<Arc<Connector>> = LazyLock::new(|| Arc::new(Connector::new()));
        SHARED.clone()
    }

    /// A connection to `peer`: one kept from a previous request (`true`), or a new one
    ///
    /// # Errors
    ///
    /// When `peer` can't be connected to, in time.
    pub async fn get_http_session(&self, peer: &HttpPeer) -> Result<(HttpSession, bool)> {
        let key = peer.reuse_hash();
        if let Some(session) = self.pool.checkout(key, peer.options.max_h2_streams) {
            return Ok((session, true));
        }
        let session = self.connect(peer, key).await.map_err(Error::into_up)?;
        Ok((session, false))
    }

    /// Keeps `session` for the next request to its upstream, for `idle_timeout`
    pub fn release_http_session(&self, session: HttpSession, idle_timeout: Option<Duration>) {
        let Sender::H1(sender) = session.sender else {
            // HTTP/2 connections stay in the pool while they're open
            return;
        };
        if sender.is_closed() {
            return;
        }
        self.pool
            .keep(session.home, session.key, sender, session.fd, idle_timeout);
    }

    /// Sends `request` with `body` to `peer`, and reads the whole response (its body is
    /// dropped): for requests whose answer only matters by its status (e.g. mirrored copies)
    ///
    /// # Errors
    ///
    /// When `peer` can't be reached, or its response read.
    pub async fn request(
        &self,
        peer: &HttpPeer,
        request: crate::http::RequestHeader,
        body: bytes::Bytes,
    ) -> Result<crate::http::ResponseHeader> {
        use http_body_util::BodyExt;

        let (mut session, _) = self.get_http_session(peer).await?;
        let mut request = request;
        crate::proxy::set_target(&mut request, session.is_http2());
        let request = hyper::Request::from_parts(request.into_parts(), UpstreamBody::full(body));
        let response = match &mut session.sender {
            Sender::H1(sender) => {
                sender
                    .ready()
                    .await
                    .or_err(ErrorType::ConnectionClosed, "sending")?;
                sender.send_request(request).await
            }
            Sender::H2(sender, _) => {
                sender
                    .ready()
                    .await
                    .or_err(ErrorType::ConnectionClosed, "sending")?;
                sender.send_request(request).await
            }
        }
        .or_err(ErrorType::ReadError, "reading the response")?;
        let (parts, body) = response.into_parts();
        body.collect()
            .await
            .or_err(ErrorType::ReadError, "reading the response body")?;
        self.release_http_session(session, peer.options.idle_timeout);
        Ok(crate::http::ResponseHeader::from_parts(parts))
    }

    async fn connect(&self, peer: &HttpPeer, key: u64) -> Result<HttpSession> {
        let options = &peer.options;
        let connecting = async {
            let (stream, fd) = tcp(peer).await?;
            if !peer.is_tls() {
                let stream: Stream = Box::new(stream);
                return Ok::<_, Box<Error>>((stream, fd));
            }
            let tls = self.tls(peer, stream).await?;
            Ok((Box::new(tls) as Stream, fd))
        };
        let (stream, fd) = match options.total_connection_timeout {
            Some(timeout) => tokio::time::timeout(timeout, connecting)
                .await
                .map_err(|_| {
                    Error::explain(ErrorType::ConnectTimedout, "connecting (with TLS)")
                })??,
            None => connecting.await?,
        };
        let sender = if stream.selected_alpn_proto() == Some(ALPN::H2) {
            let sender = handshake_h2(stream, options.h2_ping_interval).await?;
            let streams = self.pool.share(key, sender.clone(), fd);
            Sender::H2(sender, streams)
        } else {
            Sender::H1(handshake_h1(stream).await?)
        };
        Ok(HttpSession {
            sender,
            key,
            home: super::pool::own_shard(),
            fd,
        })
    }

    async fn tls(&self, peer: &HttpPeer, stream: L4Stream) -> Result<TlsStream> {
        let Some(connector) = &self.tls else {
            return Err(Error::explain(
                ErrorType::TLSHandshakeFailure,
                "no TLS to upstreams",
            ));
        };
        let options = &peer.options;
        let mut config = connector
            .configure()
            .or_err(ErrorType::TLSHandshakeFailure, "setting up TLS")?;
        config.set_verify_hostname(options.verify_hostname);
        if !options.verify_cert {
            config.set_verify(SslVerifyMode::NONE);
        }
        config.set_use_server_name_indication(!peer.sni.is_empty());
        config
            .set_alpn_protos(options.alpn.to_wire_preference())
            .or_err(ErrorType::TLSHandshakeFailure, "setting ALPN")?;
        let name = match (&options.alternative_cn, peer.sni.is_empty()) {
            (Some(name), true) => name.clone(),
            (_, false) => peer.sni.clone(),
            (None, true) => peer
                .address()
                .as_inet()
                .map(|addr| addr.ip().to_string())
                .unwrap_or_default(),
        };
        let ssl = config
            .into_ssl(&name)
            .or_err(ErrorType::TLSHandshakeFailure, "setting up TLS")?;
        let socket = stream.get_socket_digest();
        let mut tls = SslStream::new(ssl, stream).or_err(ErrorType::TLSHandshakeFailure, "TLS")?;
        std::pin::Pin::new(&mut tls)
            .connect()
            .await
            .or_err(ErrorType::TLSHandshakeFailure, "TLS connect() failed")?;
        let digest = SslDigest::from_ssl(tls.ssl());
        Ok(TlsStream::new(tls, digest, socket))
    }
}

impl Default for Connector {
    fn default() -> Self {
        Self::new()
    }
}

/// A TCP connection to `peer`, set up as its options say
async fn tcp(peer: &HttpPeer) -> Result<(L4Stream, RawFd)> {
    let options = &peer.options;
    let Some(addr) = peer.address().as_inet().copied() else {
        return Err(Error::explain(ErrorType::ConnectError, "not an IP address"));
    };
    let socket = if addr.is_ipv4() {
        TcpSocket::new_v4()
    } else {
        TcpSocket::new_v6()
    }
    .or_err(ErrorType::SocketError, "creating a socket")?;
    if let Some(size) = options.tcp_recv_buf {
        let _ = socket.set_recv_buffer_size(u32::try_from(size).unwrap_or(u32::MAX));
    }
    if let Some(bind) = options.bind_to {
        socket.bind(bind).or_err(ErrorType::BindError, "binding")?;
    }
    let connecting = socket.connect(addr);
    let stream = match options.connection_timeout {
        Some(timeout) => tokio::time::timeout(timeout, connecting)
            .await
            .map_err(|_| Error::explain(ErrorType::ConnectTimedout, "connecting"))?,
        None => connecting.await,
    }
    .map_err(connect_error)?;
    set_up(&stream, options);
    let fd = stream.as_raw_fd();
    Ok((L4Stream::from(stream), fd))
}

fn connect_error(err: std::io::Error) -> Box<Error> {
    let etype = match err.kind() {
        std::io::ErrorKind::ConnectionRefused => ErrorType::ConnectRefused,
        std::io::ErrorKind::HostUnreachable | std::io::ErrorKind::NetworkUnreachable => {
            ErrorType::ConnectNoRoute
        }
        std::io::ErrorKind::TimedOut => ErrorType::ConnectTimedout,
        _ => ErrorType::ConnectError,
    };
    Error::because(etype, "connecting", err)
}

/// TCP keepalive, as the peer's options say
fn set_up(stream: &TcpStream, options: &super::peer::PeerOptions) {
    let Some(keepalive) = &options.tcp_keepalive else {
        return;
    };
    let params = socket2::TcpKeepalive::new()
        .with_time(keepalive.idle)
        .with_interval(keepalive.interval)
        .with_retries(u32::try_from(keepalive.count).unwrap_or(u32::MAX));
    let socket = socket2::SockRef::from(stream);
    if let Err(err) = socket.set_tcp_keepalive(&params) {
        tracing::debug!("can't set TCP keepalive: {err}");
    }
    #[cfg(target_os = "linux")]
    if !keepalive.user_timeout.is_zero() {
        let _ = socket.set_tcp_user_timeout(Some(keepalive.user_timeout));
    }
}

async fn handshake_h1(stream: Stream) -> Result<http1::SendRequest<UpstreamBody>> {
    let (sender, connection) = http1::Builder::new()
        .preserve_header_case(true)
        .handshake(TokioIo::new(stream))
        .await
        .or_err(ErrorType::H1Error, "HTTP/1.1 handshake")?;
    tokio::spawn(async move {
        if let Err(err) = connection.with_upgrades().await {
            tracing::debug!("upstream connection closed: {err}");
        }
    });
    Ok(sender)
}

async fn handshake_h2(
    stream: Stream,
    ping_interval: Option<Duration>,
) -> Result<http2::SendRequest<UpstreamBody>> {
    let mut builder = http2::Builder::new(TokioExecutor::new());
    builder.timer(TokioTimer::new());
    if let Some(interval) = ping_interval {
        builder
            .keep_alive_interval(interval)
            .keep_alive_while_idle(true);
    }
    let (sender, connection) = builder
        .handshake(TokioIo::new(stream))
        .await
        .or_err(ErrorType::H2Error, "HTTP/2 handshake")?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::debug!("upstream HTTP/2 connection closed: {err}");
        }
    });
    Ok(sender)
}
