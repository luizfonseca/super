//! A service accepting connections on its addresses (with TLS or not), each served by its app

use std::{net::SocketAddr, os::fd::AsRawFd, sync::Arc};

use tokio::net::TcpListener;

use super::Service as ServiceTrait;
use crate::{
    apps::ServerApp,
    protocols::{l4::stream::Stream as L4Stream, Stream},
    server::{ListenFds, ShutdownWatch},
    tls::{Acceptor, TlsSettings},
};

/// Options of a listening socket
#[derive(Debug, Clone, Default)]
pub struct TcpSocketOptions {
    /// Only IPv6 on an IPv6 address (the system's default when `None`)
    pub ipv6_only: Option<bool>,
}

/// An address to listen on
struct Endpoint {
    address: String,
    tls: Option<Acceptor>,
    options: TcpSocketOptions,
}

/// Accepts connections on its addresses, and hands them to its app
pub struct Service<A> {
    name: String,
    app: Arc<A>,
    endpoints: Vec<Endpoint>,
    /// The threads serving its connections (the server's default when `None`)
    pub threads: Option<usize>,
}

impl<A: ServerApp> Service<A> {
    pub fn new(name: String, app: A) -> Self {
        Self {
            name,
            app: Arc::new(app),
            endpoints: vec![],
            threads: None,
        }
    }

    pub fn add_tcp(&mut self, address: &str) {
        self.add_tcp_with_settings(address, TcpSocketOptions::default());
    }

    pub fn add_tcp_with_settings(&mut self, address: &str, options: TcpSocketOptions) {
        self.endpoints.push(Endpoint {
            address: address.to_string(),
            tls: None,
            options,
        });
    }

    /// Listens on `address` with TLS
    pub fn add_tls_with_settings(
        &mut self,
        address: &str,
        options: Option<TcpSocketOptions>,
        settings: TlsSettings,
    ) {
        self.endpoints.push(Endpoint {
            address: address.to_string(),
            tls: Some(settings.build()),
            options: options.unwrap_or_default(),
        });
    }

    pub fn app_logic(&self) -> &Arc<A> {
        &self.app
    }
}

impl<A: ServerApp> ServiceTrait for Service<A> {
    async fn start_service(
        &mut self,
        fds: Option<ListenFds>,
        shutdown: ShutdownWatch,
        _listeners_per_fd: usize,
    ) {
        let accepting = self.endpoints.iter().map(|endpoint| {
            let (app, shutdown) = (self.app.clone(), shutdown.clone());
            accept(app, endpoint, fds.clone(), shutdown)
        });
        futures_util::future::join_all(accepting).await;
        self.app.cleanup().await;
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn threads(&self) -> Option<usize> {
        self.threads
    }

    fn listen_addresses(&self) -> Option<Vec<String>> {
        Some(
            self.endpoints
                .iter()
                .map(|endpoint| endpoint.address.clone())
                .collect(),
        )
    }
}

/// Accepts the connections of `endpoint` until shutting down: on each of the service's threads
/// when they don't steal work from each other (each serves what it accepts, as nginx's
/// workers), else on the service's runtime
async fn accept<A: ServerApp>(
    app: Arc<A>,
    endpoint: &Endpoint,
    fds: Option<ListenFds>,
    shutdown: ShutdownWatch,
) {
    let listener = match listen(&endpoint.address, &endpoint.options, fds.as_ref()) {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!("can't listen on {}: {err}", endpoint.address);
            // Without its addresses, the server can't do its job
            std::process::exit(1);
        }
    };
    let tls = endpoint.tls.clone();
    let Some(workers) = crate::server::workers() else {
        accept_on(app, listener, tls, shutdown).await;
        return;
    };
    // The same socket on each thread: the first one to take a connection serves it
    let accepting = workers
        .iter()
        .filter_map(|worker| {
            let listener = listener.try_clone().ok()?;
            let (app, tls, shutdown) = (app.clone(), tls.clone(), shutdown.clone());
            Some(worker.spawn(accept_on(app, listener, tls, shutdown)))
        })
        .collect::<Vec<_>>();
    futures_util::future::join_all(accepting).await;
}

/// Accepts connections on `listener`, and serves them on this thread, until shutting down
async fn accept_on<A: ServerApp>(
    app: Arc<A>,
    listener: std::net::TcpListener,
    tls: Option<Acceptor>,
    mut shutdown: ShutdownWatch,
) {
    let listener = match TcpListener::from_std(listener) {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!("can't accept connections: {err}");
            return;
        }
    };
    // Without a server to say so, it never shuts down
    let mut watching = true;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let socket = match accepted {
                    Ok((socket, _)) => socket,
                    Err(err) => {
                        tracing::error!("Accept() failed: {err}");
                        // E.g. too many open files: some may be closed soon
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        continue;
                    }
                };
                let (app, tls, shutdown) = (app.clone(), tls.clone(), shutdown.clone());
                tokio::spawn(serve(app, L4Stream::from(socket), tls, shutdown));
            }
            changed = shutdown.changed(), if watching => {
                watching = changed.is_ok();
                if watching && *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

/// Serves a connection: its TLS handshake, then its app (again as long as it hands it back)
async fn serve<A: ServerApp>(
    app: Arc<A>,
    socket: L4Stream,
    tls: Option<Acceptor>,
    shutdown: ShutdownWatch,
) {
    let mut stream: Stream = match tls {
        None => Box::new(socket),
        Some(acceptor) => match acceptor.accept(socket).await {
            Ok(tls) => Box::new(tls),
            Err(err) => {
                tracing::debug!("Downstream handshake error: {err}");
                return;
            }
        },
    };
    while let Some(again) = app.process_new(stream, &shutdown).await {
        stream = again;
    }
}

/// The listener of `address`: handed over by the previous process, or bound now
fn listen(
    address: &str,
    options: &TcpSocketOptions,
    fds: Option<&ListenFds>,
) -> std::io::Result<std::net::TcpListener> {
    if let Some(fd) = fds.and_then(|fds| fds.take(address)) {
        tracing::info!("listening on {address} (handed over)");
        let listener = fd.into_std_listener();
        listener.set_nonblocking(true)?;
        if let Some(fds) = fds {
            fds.add(address, listener.as_raw_fd());
        }
        return Ok(listener);
    }
    let addr = address
        .parse::<SocketAddr>()
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(addr),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    socket.set_reuse_address(true)?;
    if let (Some(only), true) = (options.ipv6_only, addr.is_ipv6()) {
        socket.set_only_v6(only)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(65_535)?;
    let listener = std::net::TcpListener::from(socket);
    if let Some(fds) = fds {
        fds.add(address, listener.as_raw_fd());
    }
    Ok(listener)
}
