//! HTTP/3 (`server.http3`): the certificates of its handshakes, and how clients learn of it
//! (`Alt-Svc`, on the HTTPS responses).
//!
//! QUIC's TLS (rustls) asks for a certificate without waiting, while the store is read
//! asynchronously: certificates are kept here, filled by the handshakes of HTTPS (a browser
//! always connects over TCP before it tries HTTP/3), and read again from the store once a
//! minute (renewals are picked up). A handshake for a certificate not there yet fails, and its
//! client uses HTTPS meanwhile.

use std::{
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use http::HeaderValue;
use once_cell::sync::OnceCell;
use super_core::tls::rustls::{
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};
use super_store::certificates::Certificate;

use crate::stores;

/// How long a certificate is used before it's read from the store again
const FRESH: Duration = Duration::from_secs(60);

/// How long clients remember that HTTP/3 is there (a day)
const ALT_SVC_MAX_AGE: u64 = 86_400;

/// The certificates of HTTP/3's handshakes, by name (as the store keeps them)
pub static CERTIFICATES: LazyLock<Arc<Certificates>> = LazyLock::new(Arc::default);

/// `Alt-Svc` of the HTTPS responses, by the port they're answered on (when HTTP/3 is on)
static ALT_SVC: OnceCell<Vec<(u16, HeaderValue)>> = OnceCell::new();

/// The certificates of HTTP/3's handshakes, and when they were read
#[derive(Debug, Default)]
pub struct Certificates {
    kept: papaya::HashMap<String, (Instant, Arc<CertifiedKey>)>,
}

impl Certificates {
    /// Keeps `certificate` of `name` (as the store keeps it), unless it was read lately: called
    /// with the certificates of HTTPS handshakes
    pub fn keep(&self, name: &str, certificate: &Certificate) {
        if ALT_SVC.get().is_none() {
            return;
        }
        let fresh = self
            .kept
            .pin()
            .get(name)
            .is_some_and(|(read, _)| read.elapsed() < FRESH);
        if !fresh {
            self.convert(name, certificate);
        }
    }

    fn convert(&self, name: &str, certificate: &Certificate) {
        let key = super_core::services::quic::certified_key(
            &certificate.leaf,
            &certificate.chain,
            &certificate.key,
        );
        match key {
            Ok(key) => {
                self.kept
                    .pin()
                    .insert(name.to_string(), (Instant::now(), key));
            }
            Err(err) => tracing::error!("can't use the certificate of {name:?} for HTTP/3: {err}"),
        }
    }

    /// Reads the certificate of `name` from the store, in the background
    fn refresh(name: String) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            let certificate = stores::global::get_store().get_certificate(&name).await;
            match certificate {
                Some(certificate) => CERTIFICATES.convert(&name, &certificate),
                None => tracing::warn!("no certificate for host {name:?}"),
            }
        });
    }
}

impl ResolvesServerCert for Certificates {
    /// The certificate of the name the client asks for: none for names no host with TLS serves
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let name = client_hello.server_name()?.to_lowercase();
        let served = stores::lookup::named_host(&name).is_some_and(|host| host.tls_enabled);
        if !served {
            tracing::debug!("HTTP/3 handshake for {name:?}, which no host with TLS serves");
            return None;
        }
        // A wildcard host's certificate for the names it serves (ADR 0009)
        let name = stores::lookup::certificate_name(&name).into_owned();
        let kept = self.kept.pin().get(&name).cloned();
        let Some((read, key)) = kept else {
            Self::refresh(name);
            return None;
        };
        if read.elapsed() >= FRESH {
            // Read again once, while this one is still used
            self.kept
                .pin()
                .insert(name.clone(), (Instant::now(), key.clone()));
            Self::refresh(name);
        }
        Some(key)
    }
}

/// HTTP/3 is answered on `ports` (UDP): HTTPS responses say so
pub fn announce(ports: impl IntoIterator<Item = u16>) {
    let announced = ports
        .into_iter()
        .filter_map(|port| {
            let value = format!("h3=\":{port}\"; ma={ALT_SVC_MAX_AGE}");
            Some((port, HeaderValue::from_str(&value).ok()?))
        })
        .collect();
    let _ = ALT_SVC.set(announced);
}

/// `Alt-Svc` of an HTTPS response answered on `port`, when HTTP/3 is answered there
pub fn alt_svc(port: u16) -> Option<HeaderValue> {
    ALT_SVC
        .get()?
        .iter()
        .find(|(announced, _)| *announced == port)
        .map(|(_, value)| value.clone())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_alt_svc_by_port() {
        super::announce([443, 8443]);
        assert_eq!(super::alt_svc(443).unwrap(), "h3=\":443\"; ma=86400");
        assert_eq!(super::alt_svc(8443).unwrap(), "h3=\":8443\"; ma=86400");
        assert!(super::alt_svc(80).is_none());
    }
}
