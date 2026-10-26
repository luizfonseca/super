//! A service answering HTTP/3 on its addresses (QUIC, over UDP), with rustls' TLS 1.3: each
//! request goes through the same app as those of HTTP/1.1 and HTTP/2.
//!
//! Without work stealing, each of the service's threads has a socket of its own on the address
//! (Linux, `SO_REUSEPORT`: the kernel sends the packets of a client to the same one), as each
//! accepts TCP connections. A UDP socket isn't handed over to the process replacing this one
//! (`--upgrade`), since both would read each other's packets: the new process answers HTTP/3
//! once the address is free (clients use HTTP/2 meanwhile).

use std::{net::SocketAddr, sync::Arc, time::Duration};

use super::Service as ServiceTrait;
use crate::{
    apps::{serve_quic, HttpServerApp, HttpServerOptions},
    protocols::{l4::socket::SocketAddr as Address, Digest, SocketDigest, SslDigest},
    server::{ListenFds, ShutdownWatch},
    ErrorType, OrErr, Result,
};

/// How often an address taken (by the process being replaced) is tried again
const BIND_RETRY: Duration = Duration::from_secs(1);

/// Answers HTTP/3 on its addresses, with its app
pub struct Service<A> {
    name: String,
    app: Arc<A>,
    addresses: Vec<String>,
    tls: Arc<rustls::ServerConfig>,
    /// The threads serving its connections (the server's default when `None`)
    pub threads: Option<usize>,
}

impl<A: HttpServerApp> Service<A> {
    /// A service answering with `app`, and the certificates of `certificates` (by the name a
    /// client asks for)
    ///
    /// # Errors
    ///
    /// When TLS 1.3 can't be set up.
    pub fn new(
        name: String,
        app: Arc<A>,
        certificates: Arc<dyn rustls::server::ResolvesServerCert>,
    ) -> Result<Self> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .or_err(ErrorType::InternalError, "setting up TLS 1.3")?
            .with_no_client_auth()
            .with_cert_resolver(certificates);
        tls.alpn_protocols = vec![b"h3".to_vec()];
        Ok(Self {
            name,
            app,
            addresses: vec![],
            tls: Arc::new(tls),
            threads: None,
        })
    }

    /// Answers on `address` (UDP)
    pub fn add_udp(&mut self, address: &str) {
        self.addresses.push(address.to_string());
    }

    /// Answers on `socket` until shutting down
    #[cfg(test)]
    pub(crate) fn serve_on(
        &self,
        socket: std::net::UdpSocket,
        shutdown: ShutdownWatch,
    ) -> impl std::future::Future<Output = ()> + Send + 'static {
        let config = quic_config(&self.tls, &HttpServerOptions::default()).unwrap();
        accept_on(self.app.clone(), socket, config, shutdown)
    }
}

/// A certificate (`leaf`, then `chain`) and its `key`, as QUIC's TLS takes them
///
/// # Errors
///
/// When the key isn't one rustls signs with (RSA, ECDSA P-256 or P-384, Ed25519).
pub fn certified_key(
    leaf: &openssl::x509::X509Ref,
    chain: &[openssl::x509::X509],
    key: &openssl::pkey::PKeyRef<openssl::pkey::Private>,
) -> Result<Arc<rustls::sign::CertifiedKey>> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    let der = |cert: &openssl::x509::X509Ref| {
        cert.to_der()
            .map(CertificateDer::from)
            .or_err(ErrorType::InternalError, "reading a certificate")
    };
    let certificates = std::iter::once(leaf)
        .chain(chain.iter().map(std::ops::Deref::deref))
        .map(der)
        .collect::<Result<Vec<_>>>()?;
    let key = key
        .private_key_to_pkcs8()
        .or_err(ErrorType::InternalError, "reading a private key")?;
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key));
    let key = rustls::crypto::ring::default_provider()
        .key_provider
        .load_private_key(key)
        .or_err(ErrorType::InternalError, "loading a private key")?;
    Ok(Arc::new(rustls::sign::CertifiedKey::new(certificates, key)))
}

impl<A: HttpServerApp> ServiceTrait for Service<A> {
    async fn start_service(
        &mut self,
        _fds: Option<ListenFds>,
        shutdown: ShutdownWatch,
        _listeners_per_fd: usize,
    ) {
        let options = self.app.server_options().cloned().unwrap_or_default();
        let config = match quic_config(&self.tls, &options) {
            Ok(config) => config,
            Err(err) => {
                tracing::error!("can't answer HTTP/3: {err}");
                return;
            }
        };
        let listening = self.addresses.iter().map(|address| {
            let (app, config, shutdown) = (self.app.clone(), config.clone(), shutdown.clone());
            listen(app, address, config, shutdown)
        });
        futures_util::future::join_all(listening).await;
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn threads(&self) -> Option<usize> {
        self.threads
    }

    /// None of the sockets handed over (TCP)
    fn listen_addresses(&self) -> Option<Vec<String>> {
        Some(vec![])
    }
}

/// QUIC with `tls`, and the limits of `options`: streams at once, and how long a connection
/// can stay idle
fn quic_config(
    tls: &Arc<rustls::ServerConfig>,
    options: &HttpServerOptions,
) -> Result<quinn::ServerConfig> {
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls.clone())
        .or_err(ErrorType::InternalError, "setting up QUIC's TLS")?;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    if let Some(streams) = options.max_h2_streams {
        transport.max_concurrent_bidi_streams(streams.into());
    }
    let idle = options
        .idle_timeout
        .and_then(|timeout| quinn::IdleTimeout::try_from(timeout).ok());
    transport.max_idle_timeout(idle);
    config.transport_config(Arc::new(transport));
    Ok(config)
}

/// Answers on `address` until shutting down: on each of the service's threads when they don't
/// steal work from each other (Linux), else here
async fn listen<A: HttpServerApp>(
    app: Arc<A>,
    address: &str,
    config: quinn::ServerConfig,
    mut shutdown: ShutdownWatch,
) {
    let Ok(addr) = address.parse::<SocketAddr>() else {
        tracing::error!("can't answer HTTP/3 on {address}: not an address");
        return;
    };
    let Some(socket) = free(addr, &mut shutdown).await else {
        return;
    };
    tracing::info!("answering HTTP/3 on {address}");
    #[cfg(target_os = "linux")]
    if let Some(workers) = crate::server::workers().filter(|workers| workers.len() > 1) {
        // Bound again, shared by the threads
        drop(socket);
        let endpoints = (0..workers.len())
            .filter_map(|index| {
                let socket = bind(addr, true)
                    .inspect_err(|err| tracing::error!("can't answer HTTP/3 on {addr}: {err}"))
                    .ok()?;
                let (app, config, shutdown) = (app.clone(), config.clone(), shutdown.clone());
                Some(
                    workers
                        .handle(index)?
                        .spawn(accept_on(app, socket, config, shutdown)),
                )
            })
            .collect::<Vec<_>>();
        futures_util::future::join_all(endpoints).await;
        return;
    }
    accept_on(app, socket, config, shutdown).await;
}

/// `addr`, bound once it's free (the process being replaced may answer there still), unless
/// shutting down meanwhile
async fn free(addr: SocketAddr, shutdown: &mut ShutdownWatch) -> Option<std::net::UdpSocket> {
    for attempt in 0_u64.. {
        match bind(addr, false) {
            Ok(socket) => return Some(socket),
            Err(err) if err.kind() == std::io::ErrorKind::AddrInUse && attempt == 0 => {
                tracing::info!("waiting for {addr} (UDP) to be free, to answer HTTP/3");
            }
            Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {}
            Err(err) => {
                tracing::error!("can't answer HTTP/3 on {addr}: {err}");
                return None;
            }
        }
        let stopping = async {
            // Without a server to say so, it never shuts down
            if shutdown.wait_for(|stopping| *stopping).await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            () = tokio::time::sleep(BIND_RETRY) => {}
            () = stopping => return None,
        }
    }
    None
}

/// A UDP socket on `addr`, `shared` by the threads of the service (Linux)
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
fn bind(addr: SocketAddr, shared: bool) -> std::io::Result<std::net::UdpSocket> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(addr),
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    #[cfg(target_os = "linux")]
    if shared {
        socket.set_reuse_port(true)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

/// Accepts the connections of `socket` until shutting down, then lets those open finish
async fn accept_on<A: HttpServerApp>(
    app: Arc<A>,
    socket: std::net::UdpSocket,
    config: quinn::ServerConfig,
    mut shutdown: ShutdownWatch,
) {
    let endpoint = quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(config),
        socket,
        Arc::new(quinn::TokioRuntime),
    );
    let endpoint = match endpoint {
        Ok(endpoint) => endpoint,
        Err(err) => {
            tracing::error!("can't answer HTTP/3: {err}");
            return;
        }
    };
    let local = endpoint.local_addr().ok();
    // Without a server to say so, it never shuts down
    let mut watching = true;
    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else {
                    return;
                };
                let (app, shutdown) = (app.clone(), shutdown.clone());
                tokio::spawn(async move {
                    let connection = match incoming.await {
                        Ok(connection) => connection,
                        Err(err) => {
                            tracing::debug!("QUIC handshake failed: {err}");
                            return;
                        }
                    };
                    let digest = digest(&connection, local);
                    serve_quic(&app, connection, digest, &shutdown).await;
                });
            }
            changed = shutdown.changed(), if watching => {
                watching = changed.is_ok();
                if watching && *shutdown.borrow() {
                    break;
                }
            }
        }
    }
    // No new connections: those open are told to go away, and finish their requests
    endpoint.set_server_config(None);
    endpoint.wait_idle().await;
}

/// What's known of a QUIC connection: its addresses, and its TLS (always 1.3)
fn digest(connection: &quinn::Connection, local: Option<SocketAddr>) -> Digest {
    let server_name = connection
        .handshake_data()
        .and_then(|data| data.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
        .and_then(|data| data.server_name)
        .map(|name| name.to_lowercase());
    Digest {
        ssl_digest: Some(Arc::new(SslDigest {
            cipher: "",
            version: "TLSv1.3",
            server_name,
            extension: crate::protocols::tls::SslDigestExtension::default(),
        })),
        socket_digest: Some(Arc::new(SocketDigest {
            peer_addr: Some(Address::Inet(connection.remote_address())),
            local_addr: local.map(Address::Inet),
        })),
    }
}
