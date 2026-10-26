//! The address of a connection's end: an IP address and port, or a Unix socket's path

use std::{fmt, net, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SocketAddr {
    Inet(net::SocketAddr),
    Unix(PathBuf),
}

impl SocketAddr {
    pub fn as_inet(&self) -> Option<&net::SocketAddr> {
        match self {
            SocketAddr::Inet(addr) => Some(addr),
            SocketAddr::Unix(_) => None,
        }
    }

    pub fn as_unix(&self) -> Option<&std::path::Path> {
        match self {
            SocketAddr::Inet(_) => None,
            SocketAddr::Unix(path) => Some(path),
        }
    }
}

impl From<net::SocketAddr> for SocketAddr {
    fn from(addr: net::SocketAddr) -> Self {
        SocketAddr::Inet(addr)
    }
}

impl fmt::Display for SocketAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SocketAddr::Inet(addr) => addr.fmt(f),
            SocketAddr::Unix(path) => write!(f, "unix:{}", path.to_string_lossy()),
        }
    }
}

impl std::str::FromStr for SocketAddr {
    type Err = net::AddrParseError;

    /// `unix:/path` for Unix sockets, an IP address and port otherwise
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.strip_prefix("unix:") {
            Some(path) => Ok(SocketAddr::Unix(PathBuf::from(path))),
            None => s.parse().map(SocketAddr::Inet),
        }
    }
}
