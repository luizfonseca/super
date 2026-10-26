//! Connections: their streams, and what's known of them (their addresses, their TLS)

use std::{any::Any, sync::Arc, time::Duration};

use tokio::io::{AsyncRead, AsyncWrite};

pub mod l4;
pub mod tls;

use l4::socket::SocketAddr;
pub use tls::{CustomALPN, SslDigest};

/// What's known of a connection's socket
#[derive(Debug, Clone, Default)]
pub struct SocketDigest {
    pub peer_addr: Option<SocketAddr>,
    pub local_addr: Option<SocketAddr>,
}

impl SocketDigest {
    pub fn peer_addr(&self) -> Option<&SocketAddr> {
        self.peer_addr.as_ref()
    }

    pub fn local_addr(&self) -> Option<&SocketAddr> {
        self.local_addr.as_ref()
    }
}

/// What's known of a connection, shared by its requests
#[derive(Debug, Clone, Default)]
pub struct Digest {
    pub ssl_digest: Option<Arc<SslDigest>>,
    pub socket_digest: Option<Arc<SocketDigest>>,
}

/// The protocols offered (to upstreams) or chosen (by clients) with ALPN
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ALPN {
    H1,
    H2,
    /// HTTP/2, else HTTP/1.1
    H2H1,
    /// Another protocol, of a `tcp` block
    Custom(CustomALPN),
}

impl ALPN {
    /// The protocols, in the format of TLS
    pub fn to_wire_preference(&self) -> &[u8] {
        match self {
            ALPN::H1 => b"\x08http/1.1",
            ALPN::H2 => b"\x02h2",
            ALPN::H2H1 => b"\x02h2\x08http/1.1",
            ALPN::Custom(custom) => custom.wire(),
        }
    }

    /// The protocol chosen in a handshake
    pub fn from_wire_selected(selected: &[u8]) -> Self {
        match selected {
            b"http/1.1" => ALPN::H1,
            b"h2" => ALPN::H2,
            other => ALPN::Custom(CustomALPN::new(other)),
        }
    }
}

/// TCP keepalive of connections to upstreams
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TcpKeepalive {
    pub idle: Duration,
    pub interval: Duration,
    pub count: usize,
    #[cfg(target_os = "linux")]
    pub user_timeout: Duration,
}

/// A connection: TCP or a Unix socket, with TLS or not
pub trait IO: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static {
    fn get_socket_digest(&self) -> Option<Arc<SocketDigest>>;

    fn get_ssl_digest(&self) -> Option<Arc<SslDigest>> {
        None
    }

    /// The protocol chosen with ALPN
    fn selected_alpn_proto(&self) -> Option<ALPN> {
        None
    }

    /// The stream, to downcast it into what it is
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send>;
}

/// A connection, whatever it is
pub type Stream = Box<dyn IO>;

impl dyn IO {
    /// What's known of the connection
    pub fn digest(&self) -> Digest {
        Digest {
            ssl_digest: self.get_ssl_digest(),
            socket_digest: self.get_socket_digest(),
        }
    }
}
