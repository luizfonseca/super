//! A service accepting connections on its addresses (with TLS or not), each served by its app

use std::{net::SocketAddr, os::fd::AsRawFd, sync::Arc};

use tokio::net::TcpListener;

use super::Service as ServiceTrait;
use crate::{
    apps::ServerApp,
    protocols::{l4::stream::Stream as L4Stream, Stream},
    server::{ListenFds, ShutdownWatch, Workers},
    tls::{Acceptor, TlsSettings},
};

/// Options of a listening socket
#[derive(Debug, Clone, Default)]
pub struct TcpSocketOptions {
    /// Only IPv6 on an IPv6 address (the system's default when `None`)
    pub ipv6_only: Option<bool>,
    /// Reading the PROXY protocol header of the connections of load balancers
    pub proxy_protocol: Option<ProxyProtocol>,
}

/// Reading the PROXY protocol header (v1 or v2) load balancers send first: the client it gives
/// becomes the connection's peer. Trusted peers' connections must start with one, read before
/// TLS: one without it is closed (<https://www.haproxy.org/download/3.2/doc/proxy-protocol.txt>).
#[derive(Clone)]
pub struct ProxyProtocol {
    /// Whether a peer sends headers: others' connections are served as they are, unread
    pub trusted: Arc<dyn Fn(std::net::IpAddr) -> bool + Send + Sync>,
    /// How long a header is waited for, before closing the connection
    pub timeout: std::time::Duration,
}

impl std::fmt::Debug for ProxyProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyProtocol")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
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
    let tls = (endpoint.tls.clone(), endpoint.options.proxy_protocol.clone());
    let Some(workers) = crate::server::workers() else {
        accept_on(app, listener, tls, shutdown, None).await;
        return;
    };
    // The same socket on each thread: the first one to take a connection serves it (or hands it
    // to a thread serving fewer)
    let accepting = (0..workers.len())
        .filter_map(|index| {
            let listener = listener.try_clone().ok()?;
            let (app, tls, shutdown) = (app.clone(), tls.clone(), shutdown.clone());
            let on = Some((workers, index));
            Some(
                workers
                    .handle(index)?
                    .spawn(accept_on(app, listener, tls, shutdown, on)),
            )
        })
        .collect::<Vec<_>>();
    futures_util::future::join_all(accepting).await;
}

/// Accepts connections on `listener` until shutting down, and serves them on this thread (or,
/// as thread `on` of the service's, on the one `Workers::serving` says)
async fn accept_on<A: ServerApp>(
    app: Arc<A>,
    listener: std::net::TcpListener,
    tls: (Option<Acceptor>, Option<ProxyProtocol>),
    mut shutdown: ShutdownWatch,
    on: Option<(&'static Workers, usize)>,
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
                let serving = serve(app, L4Stream::from(socket), tls, shutdown);
                spawn(serving, on.as_ref());
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

/// Serves a connection here, or on the thread of the service that serves fewer
fn spawn<F>(serving: F, on: Option<&(&'static Workers, usize)>)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let Some((workers, accepted)) = on else {
        tokio::spawn(serving);
        return;
    };
    let index = workers.serving(*accepted);
    let open = workers.open(index);
    let counted = async move {
        let _open = open;
        serving.await;
    };
    match workers.handle(index).filter(|_| index != *accepted) {
        Some(handle) => drop(handle.spawn(counted)),
        None => drop(tokio::spawn(counted)),
    }
}

/// Serves a connection: its PROXY protocol header, its TLS handshake, then its app (again as long
/// as it hands it back)
async fn serve<A: ServerApp>(
    app: Arc<A>,
    mut socket: L4Stream,
    (tls, proxy_protocol): (Option<Acceptor>, Option<ProxyProtocol>),
    shutdown: ShutdownWatch,
) {
    if let Some(proxy_protocol) = &proxy_protocol {
        if !proxy_header(&mut socket, proxy_protocol).await {
            return;
        }
    }
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

/// Reads the PROXY protocol header of a trusted peer's connection, waiting for it as long as
/// `proxy_protocol` says. `false` when there's none, in time, that follows the protocol: the
/// connection is closed.
async fn proxy_header(socket: &mut L4Stream, proxy_protocol: &ProxyProtocol) -> bool {
    if !socket.peer_ip().is_some_and(|ip| (proxy_protocol.trusted)(ip)) {
        return true;
    }
    let read = tokio::time::timeout(proxy_protocol.timeout, socket.read_proxy_header()).await;
    match read {
        Ok(Ok(())) => true,
        Ok(Err(err)) => {
            tracing::debug!("Downstream PROXY protocol error: {err}");
            false
        }
        Err(_) => {
            tracing::debug!("Downstream PROXY protocol error: no header in time");
            false
        }
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

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use super::{proxy_header, ProxyProtocol};
    use crate::protocols::l4::stream::Stream;

    fn trusting(trusted: bool, timeout: Duration) -> ProxyProtocol {
        ProxyProtocol {
            trusted: Arc::new(move |_| trusted),
            timeout,
        }
    }

    /// A connection whose client sends nothing, kept open
    async fn silent() -> (Stream, tokio::net::TcpStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap());
        let (client, accepted) = tokio::join!(client, listener.accept());
        (Stream::from(accepted.unwrap().0), client.unwrap())
    }

    #[tokio::test]
    async fn test_trusted_connections_without_a_header_in_time_are_closed() {
        // Nothing, then the timeout
        let (mut stream, _client) = silent().await;
        let proxy_protocol = trusting(true, Duration::from_millis(50));
        let waiting = proxy_header(&mut stream, &proxy_protocol);
        let served = tokio::time::timeout(Duration::from_secs(5), waiting).await;
        assert_eq!(served.ok(), Some(false));

        // Part of a header, then nothing
        let (mut stream, mut client) = silent().await;
        tokio::io::AsyncWriteExt::write_all(&mut client, b"PROXY TCP4 203.0.113.7")
            .await
            .unwrap();
        let waiting = proxy_header(&mut stream, &proxy_protocol);
        let served = tokio::time::timeout(Duration::from_secs(5), waiting).await;
        assert_eq!(served.ok(), Some(false));

        // Untrusted peers' aren't waited for at all
        let (mut stream, _client) = silent().await;
        let proxy_protocol = trusting(false, Duration::from_secs(60));
        let waiting = proxy_header(&mut stream, &proxy_protocol);
        let served = tokio::time::timeout(Duration::from_millis(500), waiting).await;
        assert_eq!(served.ok(), Some(true));
    }
}
