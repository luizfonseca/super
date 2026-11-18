//! `server.proxy_protocol`: the PROXY protocol header (v1 or v2) load balancers send first on
//! their connections, with the client they accepted them from

use std::{net::SocketAddr, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use super_core::services::listening::{ProxyProtocol as Reading, TcpSocketOptions};

use crate::ip_set::{network, IpSet};

/// How long a header is waited for by default
const DEFAULT_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ProxyProtocol {
    /// The load balancers sending it: addresses (`10.0.0.1`) and networks (`10.0.0.0/8`). Only
    /// their connections' headers are read, and one of theirs without a header is served as it
    /// is: other clients connect directly to the same addresses.
    pub trusted: Vec<String>,
    /// The listening addresses it's read on (`"0.0.0.0:443"`), of hosts and `tcp` blocks
    /// (default: all of them)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub listen: Vec<String>,
    /// How long a header is waited for, in seconds (default: 5): after, the connection is served
    /// as it is (protocols where the server speaks first)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl ProxyProtocol {
    /// # Errors
    ///
    /// When it has no trusted load balancers, or an address isn't one.
    pub fn validate(&self) -> Result<(), String> {
        self.listeners().map(|_| ())
    }

    /// Reading it on the listeners it's for
    ///
    /// # Errors
    ///
    /// As [`Self::validate`].
    pub fn listeners(&self) -> Result<ProxyProtocolListeners, String> {
        if self.trusted.is_empty() {
            return Err("trusted: the load balancers sending PROXY protocol headers".to_string());
        }
        let trusted = self
            .trusted
            .iter()
            .map(|entry| {
                network(entry).ok_or_else(|| {
                    format!("trusted: {entry:?} is not an address or a network (10.0.0.0/8)")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let listen = self
            .listen
            .iter()
            .map(|address| {
                address
                    .parse::<SocketAddr>()
                    .map_err(|_| format!("listen: {address:?} is not an address (0.0.0.0:443)"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let trusted = IpSet::new(trusted);
        let reading = Reading {
            trusted: Arc::new(move |ip| trusted.contains(ip)),
            timeout: Duration::from_secs(self.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS)),
        };
        Ok(ProxyProtocolListeners {
            reading: Some(reading),
            listen,
        })
    }
}

/// The listeners reading PROXY protocol headers, and how
#[derive(Debug, Clone, Default)]
pub struct ProxyProtocolListeners {
    /// `None`: none do
    reading: Option<Reading>,
    /// Empty: all of them
    listen: Vec<SocketAddr>,
}

impl ProxyProtocolListeners {
    /// The options of the listener on `address`
    pub fn options(&self, address: &str) -> TcpSocketOptions {
        let listens = self.listen.is_empty()
            || address
                .parse::<SocketAddr>()
                .is_ok_and(|address| self.listen.contains(&address));
        TcpSocketOptions {
            proxy_protocol: self.reading.clone().filter(|_| listens),
            ..TcpSocketOptions::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProxyProtocol;

    fn proxy_protocol(trusted: &[&str], listen: &[&str]) -> ProxyProtocol {
        ProxyProtocol {
            trusted: trusted.iter().map(ToString::to_string).collect(),
            listen: listen.iter().map(ToString::to_string).collect(),
            timeout_secs: None,
        }
    }

    #[test]
    fn test_validation() {
        assert!(proxy_protocol(&["10.0.0.0/8", "192.0.2.1"], &["0.0.0.0:443"])
            .validate()
            .is_ok());
        assert_eq!(
            proxy_protocol(&[], &[]).validate().unwrap_err(),
            "trusted: the load balancers sending PROXY protocol headers"
        );
        assert_eq!(
            proxy_protocol(&["lb.internal"], &[]).validate().unwrap_err(),
            "trusted: \"lb.internal\" is not an address or a network (10.0.0.0/8)"
        );
        assert_eq!(
            proxy_protocol(&["10.0.0.0/8"], &["443"]).validate().unwrap_err(),
            "listen: \"443\" is not an address (0.0.0.0:443)"
        );
    }

    #[test]
    fn test_listeners_reading_it() {
        let all = proxy_protocol(&["10.0.0.0/8"], &[]).listeners().unwrap();
        let reading = all.options("0.0.0.0:443").proxy_protocol.unwrap();
        assert!((reading.trusted)("10.1.2.3".parse().unwrap()));
        assert!(!(reading.trusted)("203.0.113.9".parse().unwrap()));
        assert_eq!(reading.timeout, std::time::Duration::from_secs(5));
        assert!(all.options("0.0.0.0:80").proxy_protocol.is_some());

        let some = proxy_protocol(&["10.0.0.0/8"], &["0.0.0.0:443"])
            .listeners()
            .unwrap();
        assert!(some.options("0.0.0.0:443").proxy_protocol.is_some());
        assert!(some.options("0.0.0.0:80").proxy_protocol.is_none());

        let none = super::ProxyProtocolListeners::default();
        assert!(none.options("0.0.0.0:443").proxy_protocol.is_none());
    }
}
