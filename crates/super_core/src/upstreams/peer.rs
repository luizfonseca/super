//! An upstream to send a request to, and how to connect to it

use std::{
    collections::BTreeMap,
    fmt,
    hash::{DefaultHasher, Hash, Hasher},
    net,
    time::Duration,
};

use crate::protocols::{l4::socket::SocketAddr, TcpKeepalive, ALPN};

/// How to connect to an upstream, and how long to wait for it
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerOptions {
    /// Binds connections to this address first
    pub bind_to: Option<net::SocketAddr>,
    /// For each attempt to connect (TCP)
    pub connection_timeout: Option<Duration>,
    /// For connecting, TLS handshake included
    pub total_connection_timeout: Option<Duration>,
    /// For each read of the response (its header, then each part of its body)
    pub read_timeout: Option<Duration>,
    /// For each write of the request
    pub write_timeout: Option<Duration>,
    /// How long a connection is kept for the next request
    pub idle_timeout: Option<Duration>,
    pub verify_cert: bool,
    pub verify_hostname: bool,
    /// Another name the certificate can be for
    pub alternative_cn: Option<String>,
    /// The protocols offered with ALPN (TLS only: plain connections are HTTP/1.1)
    pub alpn: ALPN,
    pub tcp_keepalive: Option<TcpKeepalive>,
    pub tcp_recv_buf: Option<usize>,
    pub tcp_fast_open: bool,
    /// How often HTTP/2 connections are pinged
    pub h2_ping_interval: Option<Duration>,
    /// Streams an HTTP/2 connection carries at once before another one is opened
    pub max_h2_streams: usize,
    /// Headers of a CONNECT through a proxy (unused)
    pub extra_proxy_headers: BTreeMap<String, Vec<u8>>,
}

impl PeerOptions {
    pub fn new() -> Self {
        Self {
            bind_to: None,
            connection_timeout: None,
            total_connection_timeout: None,
            read_timeout: None,
            write_timeout: None,
            idle_timeout: None,
            verify_cert: true,
            verify_hostname: true,
            alternative_cn: None,
            alpn: ALPN::H1,
            tcp_keepalive: None,
            tcp_recv_buf: None,
            tcp_fast_open: false,
            h2_ping_interval: None,
            max_h2_streams: 1,
            extra_proxy_headers: BTreeMap::new(),
        }
    }
}

impl Default for PeerOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// What connections to an upstream are, to reuse them
pub trait Peer {
    fn address(&self) -> &SocketAddr;

    fn tls(&self) -> bool;

    fn sni(&self) -> &str;

    /// Connections to peers with the same hash are reused for one another
    fn reuse_hash(&self) -> u64;
}

/// An HTTP upstream: its address, whether it's HTTPS (and the name asked for), and options
#[derive(Debug, Clone)]
pub struct HttpPeer {
    pub address: SocketAddr,
    pub scheme: Scheme,
    pub sni: String,
    pub options: PeerOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scheme {
    HTTP,
    HTTPS,
}

impl HttpPeer {
    pub fn new(address: impl Into<SocketAddr>, tls: bool, sni: String) -> Self {
        Self {
            address: address.into(),
            scheme: if tls { Scheme::HTTPS } else { Scheme::HTTP },
            sni,
            options: PeerOptions::new(),
        }
    }

    pub fn is_tls(&self) -> bool {
        self.scheme == Scheme::HTTPS
    }
}

impl Peer for HttpPeer {
    fn address(&self) -> &SocketAddr {
        &self.address
    }

    fn tls(&self) -> bool {
        self.is_tls()
    }

    fn sni(&self) -> &str {
        &self.sni
    }

    fn reuse_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.address.hash(&mut hasher);
        self.scheme.hash(&mut hasher);
        self.sni.hash(&mut hasher);
        // Connections set up differently aren't the same
        let options = &self.options;
        (
            options.verify_cert,
            options.verify_hostname,
            &options.alternative_cn,
            &options.alpn,
            options.bind_to,
        )
            .hash(&mut hasher);
        hasher.finish()
    }
}

impl fmt::Display for HttpPeer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "addr: {}, scheme: {:?}", self.address, self.scheme)?;
        if !self.sni.is_empty() {
            write!(f, ", sni: {}", self.sni)?;
        }
        Ok(())
    }
}

impl From<net::SocketAddr> for HttpPeer {
    fn from(addr: net::SocketAddr) -> Self {
        Self::new(addr, false, String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::{HttpPeer, Peer};

    #[test]
    fn test_connections_are_reused_for_the_same_peer() {
        let peer = |address: &str, tls, sni: &str| {
            HttpPeer::new(
                address.parse::<std::net::SocketAddr>().unwrap(),
                tls,
                sni.to_string(),
            )
        };
        let a = peer("10.0.0.1:443", true, "a.test");
        assert_eq!(
            a.reuse_hash(),
            peer("10.0.0.1:443", true, "a.test").reuse_hash()
        );
        assert_ne!(
            a.reuse_hash(),
            peer("10.0.0.1:443", true, "b.test").reuse_hash()
        );
        assert_ne!(
            a.reuse_hash(),
            peer("10.0.0.2:443", true, "a.test").reuse_hash()
        );

        let mut unverified = a.clone();
        unverified.options.verify_cert = false;
        assert_ne!(a.reuse_hash(), unverified.reuse_hash());
        assert_eq!(
            a.to_string(),
            "addr: 10.0.0.1:443, scheme: HTTPS, sni: a.test"
        );
    }
}
