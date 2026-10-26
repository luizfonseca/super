//! Services the server runs, each on its own threads: listening ones ([`listening::Service`]),
//! and background ones. A service can wait for others to be ready before it starts.

use std::future::Future;

use tokio::sync::watch;

use crate::server::{ListenFds, ShutdownWatch};

pub mod listening;

/// A service that starts right away
pub trait Service: Send + Sync + 'static {
    /// Runs the service, with the listening sockets handed over by a previous process (`fds`),
    /// until `shutdown`
    fn start_service(
        &mut self,
        fds: Option<ListenFds>,
        shutdown: ShutdownWatch,
        listeners_per_fd: usize,
    ) -> impl Future<Output = ()> + Send;

    fn name(&self) -> &str;

    /// The threads it runs on (the server's default when `None`)
    fn threads(&self) -> Option<usize> {
        None
    }

    /// The addresses it listens on (`None` for those of the server's configuration)
    fn listen_addresses(&self) -> Option<Vec<String>> {
        None
    }
}

/// A service others can wait for: it says when it's ready
pub trait ServiceWithDependents: Send + Sync + 'static {
    fn start_service(
        &mut self,
        fds: Option<ListenFds>,
        shutdown: ShutdownWatch,
        listeners_per_fd: usize,
        ready_notifier: ServiceReadyNotifier,
    ) -> impl Future<Output = ()> + Send;

    fn name(&self) -> &str;

    fn threads(&self) -> Option<usize> {
        None
    }

    fn listen_addresses(&self) -> Option<Vec<String>> {
        None
    }
}

impl<S: Service> ServiceWithDependents for S {
    async fn start_service(
        &mut self,
        fds: Option<ListenFds>,
        shutdown: ShutdownWatch,
        listeners_per_fd: usize,
        ready_notifier: ServiceReadyNotifier,
    ) {
        ready_notifier.notify_ready();
        Service::start_service(self, fds, shutdown, listeners_per_fd).await;
    }

    fn name(&self) -> &str {
        Service::name(self)
    }

    fn threads(&self) -> Option<usize> {
        Service::threads(self)
    }

    fn listen_addresses(&self) -> Option<Vec<String>> {
        Service::listen_addresses(self)
    }
}

/// Tells the services waiting for a service that it's ready
pub struct ServiceReadyNotifier(watch::Sender<bool>);

impl ServiceReadyNotifier {
    pub(crate) fn new(ready: watch::Sender<bool>) -> Self {
        Self(ready)
    }

    pub fn notify_ready(self) {
        self.0.send_replace(true);
    }
}

/// A service added to the server, that others can depend on
#[derive(Clone)]
pub struct ServiceHandle {
    pub(crate) id: usize,
    pub(crate) name: String,
    pub(crate) ready: watch::Receiver<bool>,
    pub(crate) dependencies: std::sync::Arc<std::sync::Mutex<Vec<watch::Receiver<bool>>>>,
}

impl ServiceHandle {
    /// This service starts once `dependency` is ready
    pub fn add_dependency(&self, dependency: &ServiceHandle) {
        if let Ok(mut dependencies) = self.dependencies.lock() {
            dependencies.push(dependency.ready.clone());
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn id(&self) -> usize {
        self.id
    }
}

/// A service of any type, as the server keeps them
pub trait AnyService: Send + Sync + 'static {
    fn start(
        self: Box<Self>,
        fds: Option<ListenFds>,
        shutdown: ShutdownWatch,
        ready_notifier: ServiceReadyNotifier,
    ) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>>;

    fn service_name(&self) -> String;

    fn service_threads(&self) -> Option<usize>;
}

impl<S: ServiceWithDependents> AnyService for S {
    fn start(
        mut self: Box<Self>,
        fds: Option<ListenFds>,
        shutdown: ShutdownWatch,
        ready_notifier: ServiceReadyNotifier,
    ) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            ServiceWithDependents::start_service(&mut *self, fds, shutdown, 1, ready_notifier)
                .await;
        })
    }

    fn service_name(&self) -> String {
        ServiceWithDependents::name(self).to_string()
    }

    fn service_threads(&self) -> Option<usize> {
        ServiceWithDependents::threads(self)
    }
}

/// A service, whatever its type (e.g. in a list of services of different types)
pub type BoxedService = Box<dyn AnyService>;

/// `service`, boxed
pub fn boxed(service: impl ServiceWithDependents) -> BoxedService {
    Box::new(service)
}
