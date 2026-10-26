//! Addresses with TLS passthrough blocks (ADR 0003): the server name is read from the
//! `ClientHello` before any handshake, then the connection is forwarded untouched to its block,
//! or (on an HTTPS address) handshaken by Super and served like any HTTPS connection

use std::sync::Arc;

use super_core::{
    apps::ServerApp,
    protocols::{l4::stream::Stream as L4Stream, Stream},
    server::ShutdownWatch,
    tls::{
        client_hello::{read_hello, Hello},
        handshake_with_callback,
        ssl::SslAcceptor,
    },
};

use crate::proxy_server::{cert_store::CertStore, services::HttpsApp};

use super::{passthrough_for, proxy::TcpProxy};

pub struct SniRouter {
    tcp: TcpProxy,
    /// On an HTTPS address: TLS for the other names, and what serves them
    https: Option<(SslAcceptor, Arc<HttpsApp>)>,
    certificates: CertStore,
}

impl SniRouter {
    pub fn new(tcp: TcpProxy, https: Option<(SslAcceptor, Arc<HttpsApp>)>) -> Self {
        Self {
            tcp,
            https,
            certificates: CertStore::new(),
        }
    }
}

impl ServerApp for SniRouter {
    async fn process_new(
        self: &Arc<Self>,
        stream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        let local = stream
            .get_socket_digest()
            .and_then(|digest| digest.local_addr().and_then(|addr| addr.as_inet()).copied())?;
        // The listener's own stream type (a plain TCP address)
        let Ok(mut socket) = stream.into_any().downcast::<L4Stream>() else {
            return None;
        };
        let Hello::ClientHello(server_name) = read_hello(&mut socket).await? else {
            return None;
        };

        let target = passthrough_for(
            self.tcp.targets(),
            &local,
            server_name.as_deref(),
            self.https.is_some(),
        );
        if let Some(target) = target {
            self.tcp.forward(socket, target, server_name).await;
            return None;
        }

        let (acceptor, https) = self.https.as_ref()?;
        let tls = match handshake_with_callback(acceptor, *socket, Some(&self.certificates)).await {
            Ok(tls) => tls,
            Err(err) => {
                tracing::debug!("TLS handshake failed on {local}: {err}");
                return None;
            }
        };
        // Served like a connection of the HTTPS listener, including the following requests
        // of a reused connection
        let mut stream: Stream = Box::new(tls);
        loop {
            stream = https.process_new(stream, shutdown).await?;
        }
    }
}
