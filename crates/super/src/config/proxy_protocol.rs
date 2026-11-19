//! `server.proxy_protocol`: the PROXY protocol header (v1 or v2) load balancers send first on
//! their connections, with the client they accepted them from

use std::{net::SocketAddr, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use super_core::services::listening::{self, TcpSocketOptions};

use crate::{config::listeners::Listeners, ip_set::IpSet};

/// How long a header is waited for by default
const DEFAULT_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ProxyProtocol {
    /// The load balancers sending it: addresses (`10.0.0.1`) and networks (`10.0.0.0/8`). Only
    /// their connections' headers are read, and one of theirs without a header is served as it
    /// is: other clients connect directly to the same addresses.
    pub trusted: Vec<String>,
    /// The listeners it's enabled on, by the address hosts and `tcp` blocks listen on
    /// (`"0.0.0.0:443"`; default: all of them)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub listeners: Vec<String>,
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

    /// The listeners it's enabled on, and how it's read on them
    ///
    /// # Errors
    ///
    /// As [`Self::validate`].
    pub fn listeners(&self) -> Result<ProxyProtocolListeners, String> {
        if self.trusted.is_empty() {
            return Err("trusted: the load balancers sending PROXY protocol headers".to_string());
        }
        if self.timeout_secs == Some(0) {
            return Err("timeout_secs: at least 1, for headers to arrive".to_string());
        }
        let trusted = IpSet::parse(&self.trusted).map_err(|err| format!("trusted: {err}"))?;
        let mut addresses = Vec::with_capacity(self.listeners.len());
        for address in &self.listeners {
            let parsed = address.parse::<SocketAddr>().map_err(|_| {
                format!("listeners: {address:?} is not a listening address (0.0.0.0:443)")
            })?;
            addresses.push(parsed);
        }
        let settings = listening::ProxyProtocol {
            trusted: Arc::new(move |ip| trusted.contains(ip)),
            timeout: Duration::from_secs(self.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS)),
        };
        Ok(ProxyProtocolListeners {
            settings: Some(settings),
            addresses,
        })
    }
}

impl ProxyProtocol {
    /// Whether its `listeners` are Super's: one that isn't would never read a header, and its
    /// load balancer's connections would fail
    ///
    /// # Errors
    ///
    /// When one is the address of no host or `tcp` block.
    pub fn check_listeners(&self, listeners: &Listeners) -> Result<(), String> {
        let listening = listeners
            .http
            .iter()
            .chain(&listeners.https)
            .chain(listeners.tcp.iter().map(|tcp| &tcp.listen))
            .filter_map(|address| address.parse::<SocketAddr>().ok())
            .collect::<Vec<_>>();
        let unknown = self.listeners.iter().find(|address| {
            !address
                .parse::<SocketAddr>()
                .is_ok_and(|address| listening.contains(&address))
        });
        match unknown {
            Some(address) => Err(format!(
                "listeners: {address:?} is the address of no host or `tcp` block"
            )),
            None => Ok(()),
        }
    }
}

/// The listeners the PROXY protocol is enabled on
#[derive(Debug, Clone, Default)]
pub struct ProxyProtocolListeners {
    /// The trusted load balancers and the timeout (`None`: it's off everywhere)
    settings: Option<listening::ProxyProtocol>,
    /// Their addresses (empty: all of them)
    addresses: Vec<SocketAddr>,
}

impl ProxyProtocolListeners {
    /// The options of the listener on `address`
    pub fn options(&self, address: &str) -> TcpSocketOptions {
        let enabled = self.addresses.is_empty()
            || address
                .parse::<SocketAddr>()
                .is_ok_and(|address| self.addresses.contains(&address));
        TcpSocketOptions {
            proxy_protocol: self.settings.clone().filter(|_| enabled),
            ..TcpSocketOptions::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProxyProtocol;

    fn proxy_protocol(trusted: &[&str], listeners: &[&str]) -> ProxyProtocol {
        ProxyProtocol {
            trusted: trusted.iter().map(ToString::to_string).collect(),
            listeners: listeners.iter().map(ToString::to_string).collect(),
            timeout_secs: None,
        }
    }

    #[test]
    fn test_validation() {
        assert!(
            proxy_protocol(&["10.0.0.0/8", "192.0.2.1"], &["0.0.0.0:443"])
                .validate()
                .is_ok()
        );
        assert_eq!(
            proxy_protocol(&[], &[]).validate().unwrap_err(),
            "trusted: the load balancers sending PROXY protocol headers"
        );
        assert_eq!(
            proxy_protocol(&["lb.internal"], &[])
                .validate()
                .unwrap_err(),
            "trusted: \"lb.internal\" is not an address or a network (10.0.0.0/8)"
        );
        assert_eq!(
            proxy_protocol(&["10.0.0.0/8"], &["443"])
                .validate()
                .unwrap_err(),
            "listeners: \"443\" is not a listening address (0.0.0.0:443)"
        );
        let mut instant = proxy_protocol(&["10.0.0.0/8"], &[]);
        instant.timeout_secs = Some(0);
        assert_eq!(
            instant.validate().unwrap_err(),
            "timeout_secs: at least 1, for headers to arrive"
        );
    }

    #[test]
    fn test_its_listeners_are_super_s() {
        let listeners = crate::config::listeners::Listeners {
            https: ["0.0.0.0:443".to_string()].into(),
            http: ["0.0.0.0:80".to_string()].into(),
            tcp: vec![],
            admin: None,
            metrics: None,
        };
        let on = |addresses: &[&str]| proxy_protocol(&["10.0.0.0/8"], addresses);
        assert!(on(&[]).check_listeners(&listeners).is_ok());
        assert!(on(&["0.0.0.0:443", "0.0.0.0:80"])
            .check_listeners(&listeners)
            .is_ok());
        assert_eq!(
            on(&["[::]:443"]).check_listeners(&listeners).unwrap_err(),
            "listeners: \"[::]:443\" is the address of no host or `tcp` block"
        );
    }

    #[test]
    fn test_the_listeners_it_is_enabled_on() {
        let all = proxy_protocol(&["10.0.0.0/8"], &[]).listeners().unwrap();
        let settings = all.options("0.0.0.0:443").proxy_protocol.unwrap();
        assert!((settings.trusted)("10.1.2.3".parse().unwrap()));
        assert!(!(settings.trusted)("203.0.113.9".parse().unwrap()));
        assert_eq!(settings.timeout, std::time::Duration::from_secs(5));
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
