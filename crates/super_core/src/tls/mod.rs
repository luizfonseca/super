//! TLS with OpenSSL: the settings of listeners, and their handshakes. A client's certificate is
//! chosen by the name it asks for, read from its `ClientHello` before the handshake starts (so
//! it can be looked up asynchronously, e.g. in a shared store).

use std::{
    any::Any,
    ops::{Deref, DerefMut},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use openssl::ssl::{AlpnError, SslAcceptor, SslAcceptorBuilder, SslMethod, SslRef};
pub use openssl::{hash, pkey, x509};
/// rustls, the TLS of HTTP/3 (QUIC)
pub use rustls;
use tokio_openssl::SslStream;

use crate::{
    protocols::{l4::stream::Stream as L4Stream, tls::TlsStream, SslDigest},
    Error, ErrorType, OrErr, Result,
};

pub mod client_hello;
pub mod ext;

/// OpenSSL's TLS
pub mod ssl {
    pub use openssl::ssl::*;
}

/// How long a handshake can take once the `ClientHello` arrived
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);

/// What a listener does during its handshakes
#[async_trait]
pub trait TlsAccept {
    /// Sets the certificate for the name the client asked for (`server_name`), before the
    /// handshake
    async fn certificate_callback(&self, _ssl: &mut SslRef, _server_name: Option<&str>) {}

    /// Once the handshake completed: what to attach to the connection (read through its
    /// [`SslDigest::extension`])
    async fn handshake_complete_callback(
        &self,
        _ssl: &SslRef,
    ) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

/// The TLS settings of a listener: OpenSSL's acceptor, and what it does during handshakes
pub struct TlsSettings {
    builder: SslAcceptorBuilder,
    callbacks: Option<Box<dyn TlsAccept + Send + Sync>>,
}

impl TlsSettings {
    /// Settings whose certificates `callbacks` set, for each handshake
    ///
    /// # Errors
    ///
    /// When OpenSSL can't be set up.
    pub fn with_callbacks(callbacks: Box<dyn TlsAccept + Send + Sync>) -> Result<Self> {
        let builder = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls())
            .or_err(ErrorType::InternalError, "setting up TLS")?;
        Ok(Self {
            builder,
            callbacks: Some(callbacks),
        })
    }

    /// Offers HTTP/2, then HTTP/1.1, to clients
    pub fn enable_h2(&mut self) {
        self.builder.set_alpn_select_callback(|_, offered| {
            openssl::ssl::select_next_proto(b"\x02h2\x08http/1.1", offered).ok_or(AlpnError::NOACK)
        });
    }

    /// The acceptor of these settings, and what it does during handshakes
    pub fn build(self) -> Acceptor {
        Acceptor {
            acceptor: self.builder.build(),
            callbacks: self.callbacks.map(Arc::from),
        }
    }
}

impl Deref for TlsSettings {
    type Target = SslAcceptorBuilder;

    fn deref(&self) -> &Self::Target {
        &self.builder
    }
}

impl DerefMut for TlsSettings {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.builder
    }
}

/// Handshakes the connections of a listener
#[derive(Clone)]
pub struct Acceptor {
    acceptor: SslAcceptor,
    callbacks: Option<Arc<dyn TlsAccept + Send + Sync>>,
}

impl Acceptor {
    /// The TLS stream of `stream`, once its handshake completed
    ///
    /// # Errors
    ///
    /// When the client doesn't send a `ClientHello` or the handshake fails.
    pub async fn accept(&self, stream: L4Stream) -> Result<TlsStream> {
        let callbacks = self.callbacks.as_deref();
        handshake_with_callback(&self.acceptor, stream, callbacks).await
    }
}

/// Handshakes `stream`, with the certificate `callbacks` set for the name in its `ClientHello`
///
/// # Errors
///
/// When the client doesn't send a `ClientHello` or the handshake fails.
pub async fn handshake_with_callback(
    acceptor: &SslAcceptor,
    mut stream: L4Stream,
    callbacks: Option<&(dyn TlsAccept + Send + Sync)>,
) -> Result<TlsStream> {
    let mut ssl = openssl::ssl::Ssl::new(acceptor.context())
        .or_err(ErrorType::TLSHandshakeFailure, "creating the TLS session")?;
    if let Some(callbacks) = callbacks {
        let hello = client_hello::read_hello(&mut stream).await;
        let Some(client_hello::Hello::ClientHello(server_name)) = hello else {
            return Err(
                Error::explain(ErrorType::TLSHandshakeFailure, "no ClientHello").into_down(),
            );
        };
        callbacks
            .certificate_callback(&mut ssl, server_name.as_deref())
            .await;
    }

    let socket = crate::protocols::IO::get_socket_digest(&stream);
    let mut tls = SslStream::new(ssl, stream)
        .or_err(ErrorType::TLSHandshakeFailure, "creating the TLS stream")?;
    let handshake = std::pin::Pin::new(&mut tls).accept();
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            return Err(
                Error::because(ErrorType::TLSHandshakeFailure, "TLS accept() failed", err)
                    .into_down(),
            )
        }
        Err(_) => {
            return Err(Error::explain(ErrorType::TLSHandshakeTimedout, "TLS accept()").into_down())
        }
    }

    let mut digest = SslDigest::from_ssl(tls.ssl());
    if let Some(callbacks) = callbacks {
        if let Some(extension) = callbacks.handshake_complete_callback(tls.ssl()).await {
            digest.extension.set(extension);
        }
    }
    Ok(TlsStream::new(tls, digest, socket))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{any::Any, sync::Arc};

    use async_trait::async_trait;
    use openssl::{
        asn1::Asn1Time,
        ec::{EcGroup, EcKey},
        hash::MessageDigest,
        nid::Nid,
        pkey::{PKey, Private},
        ssl::{SslConnector, SslMethod, SslRef, SslVerifyMode},
        x509::{X509NameBuilder, X509},
    };

    use super::{TlsAccept, TlsSettings};
    use crate::protocols::{l4::stream::Stream, ALPN, IO};

    /// A self-signed certificate for `name`
    pub(crate) fn certificate(name: &str) -> (X509, PKey<Private>) {
        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
        let key = PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
        let mut subject = X509NameBuilder::new().unwrap();
        subject.append_entry_by_text("CN", name).unwrap();
        let subject = subject.build();
        let mut cert = X509::builder().unwrap();
        cert.set_version(2).unwrap();
        cert.set_subject_name(&subject).unwrap();
        cert.set_issuer_name(&subject).unwrap();
        cert.set_pubkey(&key).unwrap();
        cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
            .unwrap();
        cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
            .unwrap();
        cert.sign(&key, MessageDigest::sha256()).unwrap();
        (cert.build(), key)
    }

    /// Serves a certificate for any name, and attaches the name asked for
    struct Certificates;

    struct Asked(String);

    #[async_trait]
    impl TlsAccept for Certificates {
        async fn certificate_callback(&self, ssl: &mut SslRef, server_name: Option<&str>) {
            let (cert, key) = certificate(server_name.unwrap_or("none"));
            super::ext::ssl_use_certificate(ssl, &cert).unwrap();
            super::ext::ssl_use_private_key(ssl, &key).unwrap();
        }

        async fn handshake_complete_callback(
            &self,
            ssl: &SslRef,
        ) -> Option<Arc<dyn Any + Send + Sync>> {
            let name = ssl.servername(openssl::ssl::NameType::HOST_NAME)?;
            Some(Arc::new(Asked(name.to_string())))
        }
    }

    #[tokio::test]
    async fn test_certificates_by_the_name_asked_for() {
        let mut settings = TlsSettings::with_callbacks(Box::new(Certificates)).unwrap();
        settings.enable_h2();
        let acceptor = settings.build();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let client = tokio::spawn(async move {
            let mut connector = SslConnector::builder(SslMethod::tls()).unwrap();
            connector.set_verify(SslVerifyMode::NONE);
            connector.set_alpn_protos(b"\x02h2\x08http/1.1").unwrap();
            let ssl = connector
                .build()
                .configure()
                .unwrap()
                .into_ssl("A.Test")
                .unwrap();
            let socket = tokio::net::TcpStream::connect(addr).await.unwrap();
            let mut tls = tokio_openssl::SslStream::new(ssl, socket).unwrap();
            std::pin::Pin::new(&mut tls).connect().await.unwrap();
            let cert = tls.ssl().peer_certificate().unwrap();
            let name = cert.subject_name().entries().next().unwrap();
            name.data().to_string().unwrap()
        });

        let (socket, _) = listener.accept().await.unwrap();
        let tls = acceptor.accept(Stream::from(socket)).await.unwrap();
        assert_eq!(client.await.unwrap(), "a.test");

        let digest = tls.get_ssl_digest().unwrap();
        assert_eq!(digest.server_name.as_deref(), Some("a.test"));
        assert_eq!(digest.extension.get::<Asked>().unwrap().0, "A.Test");
        assert_eq!(tls.selected_alpn_proto(), Some(ALPN::H2));
        assert!(tls.get_socket_digest().unwrap().peer_addr().is_some());
    }

    #[tokio::test]
    async fn test_connections_that_arent_tls() {
        let acceptor = TlsSettings::with_callbacks(Box::new(Certificates))
            .unwrap()
            .build();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
            socket.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        });
        let (socket, _) = listener.accept().await.unwrap();
        assert!(acceptor.accept(Stream::from(socket)).await.is_err());
    }
}
