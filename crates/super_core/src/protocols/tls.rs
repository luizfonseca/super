//! TLS connections: what their handshake chose, and the stream after it

use std::{
    any::Any,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_openssl::SslStream;

use super::{l4::stream::Stream as L4Stream, SocketDigest, ALPN, IO};

/// A protocol of `tcp` blocks, chosen with ALPN
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CustomALPN {
    /// The protocol, with its length first (the format of TLS)
    wire: Box<[u8]>,
}

impl CustomALPN {
    pub fn new(protocol: &[u8]) -> Self {
        let len = u8::try_from(protocol.len()).unwrap_or(u8::MAX);
        let wire = std::iter::once(len)
            .chain(protocol.iter().copied().take(usize::from(len)))
            .collect();
        Self { wire }
    }

    pub fn protocol(&self) -> &[u8] {
        self.wire.get(1..).unwrap_or_default()
    }

    pub fn wire(&self) -> &[u8] {
        &self.wire
    }
}

/// What a value attached to a handshake (by [`crate::tls::TlsAccept`]) is, read by type
#[derive(Clone, Default)]
pub struct SslDigestExtension {
    value: Option<Arc<dyn Any + Send + Sync>>,
}

impl SslDigestExtension {
    pub fn set(&mut self, value: Arc<dyn Any + Send + Sync>) {
        self.value = Some(value);
    }

    pub fn get<T: Any>(&self) -> Option<&T> {
        self.value.as_ref()?.downcast_ref()
    }
}

impl std::fmt::Debug for SslDigestExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SslDigestExtension").finish_non_exhaustive()
    }
}

/// What a TLS handshake chose
#[derive(Debug, Clone)]
pub struct SslDigest {
    pub cipher: &'static str,
    pub version: &'static str,
    pub server_name: Option<String>,
    pub extension: SslDigestExtension,
}

impl SslDigest {
    pub fn from_ssl(ssl: &openssl::ssl::SslRef) -> Self {
        Self {
            cipher: ssl.current_cipher().map_or("", |cipher| cipher.name()),
            version: ssl.version_str(),
            server_name: ssl
                .servername(openssl::ssl::NameType::HOST_NAME)
                .map(str::to_lowercase),
            extension: SslDigestExtension::default(),
        }
    }
}

/// A connection, after its TLS handshake
pub struct TlsStream<S = L4Stream> {
    inner: SslStream<S>,
    digest: Arc<SslDigest>,
    alpn: Option<ALPN>,
    socket: Option<Arc<SocketDigest>>,
}

impl<S> TlsStream<S> {
    pub fn new(inner: SslStream<S>, digest: SslDigest, socket: Option<Arc<SocketDigest>>) -> Self {
        let alpn = inner
            .ssl()
            .selected_alpn_protocol()
            .map(ALPN::from_wire_selected);
        Self {
            inner,
            digest: Arc::new(digest),
            alpn,
            socket,
        }
    }

    pub fn ssl(&self) -> &openssl::ssl::SslRef {
        self.inner.ssl()
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for TlsStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for TlsStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static> IO for TlsStream<S> {
    fn get_socket_digest(&self) -> Option<Arc<SocketDigest>> {
        self.socket.clone()
    }

    fn get_ssl_digest(&self) -> Option<Arc<SslDigest>> {
        Some(self.digest.clone())
    }

    fn selected_alpn_proto(&self) -> Option<ALPN> {
        self.alpn.clone()
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::CustomALPN;
    use crate::protocols::ALPN;

    #[test]
    fn test_protocols_in_the_format_of_tls() {
        let custom = CustomALPN::new(b"game/1");
        assert_eq!(custom.protocol(), b"game/1");
        assert_eq!(custom.wire(), b"\x06game/1");
        assert_eq!(ALPN::from_wire_selected(b"h2"), ALPN::H2);
        assert_eq!(ALPN::from_wire_selected(b"game/1"), ALPN::Custom(custom));
    }
}
