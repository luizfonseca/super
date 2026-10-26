//! The server: its services, each on its own threads, started once the services they depend on
//! are ready, and stopped by signals, as Pingora's:
//!
//! - `SIGTERM`: stops accepting, and exits once the services are done (or the grace period ends)
//! - `SIGQUIT`: the same, after handing the listening sockets over to a new process (`--upgrade`)
//! - `SIGINT`: exits right away

use std::{
    collections::HashMap,
    os::fd::{OwnedFd, RawFd},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use tokio::sync::watch;

use crate::services::{AnyService, BoxedService, ServiceHandle, ServiceReadyNotifier};

pub mod configuration;
mod transfer;

use configuration::{Opt, ServerConf};

/// How long a process keeps accepting after handing its listening sockets over (Pingora's)
const ACCEPTING_AFTER_UPGRADE: Duration = Duration::from_secs(5);

/// Whether the server is shutting down (`true`), for services to stop
pub type ShutdownWatch = watch::Receiver<bool>;

/// A listening socket handed over by the previous process
pub struct HandedOver(OwnedFd);

impl HandedOver {
    pub fn into_std_listener(self) -> std::net::TcpListener {
        std::net::TcpListener::from(self.0)
    }
}

#[derive(Default)]
struct Fds {
    /// Handed over by the previous process, not listened on yet
    handed_over: HashMap<String, OwnedFd>,
    /// Listened on, to hand over to the next process
    listening: Vec<(String, RawFd)>,
}

/// The listening sockets of the server, handed over between processes
#[derive(Clone, Default)]
pub struct ListenFds(Arc<Mutex<Fds>>);

impl ListenFds {
    fn fds(&self) -> std::sync::MutexGuard<'_, Fds> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The socket the previous process listened on `address` with
    pub fn take(&self, address: &str) -> Option<HandedOver> {
        self.fds().handed_over.remove(address).map(HandedOver)
    }

    /// `fd` listens on `address`
    pub fn add(&self, address: &str, fd: RawFd) {
        self.fds().listening.push((address.to_string(), fd));
    }

    fn listening(&self) -> Vec<(String, RawFd)> {
        self.fds().listening.clone()
    }
}

/// A service to start, once the services it depends on are ready
struct Pending {
    service: BoxedService,
    handle: ServiceHandle,
    ready: watch::Sender<bool>,
}

/// The server and its services
pub struct Server {
    pub configuration: Arc<ServerConf>,
    options: Opt,
    services: Vec<Pending>,
    fds: ListenFds,
    shutdown: watch::Sender<bool>,
}

impl Server {
    pub fn new_with_opt_and_conf(options: Opt, configuration: ServerConf) -> Self {
        Self {
            configuration: Arc::new(configuration),
            options,
            services: vec![],
            fds: ListenFds::default(),
            shutdown: watch::channel(false).0,
        }
    }

    /// Before anything runs: runs in the background (`daemon`), and takes the listening sockets
    /// of the process it replaces (`upgrade`)
    pub fn bootstrap(&mut self) {
        if self.configuration.daemon {
            daemonize(&self.configuration.pid_file);
        }
        if !self.options.upgrade {
            return;
        }
        let timeout = Duration::from_secs(self.configuration.upgrade_timeout_seconds);
        match transfer::receive(&self.configuration.upgrade_sock, timeout) {
            Ok(received) => {
                tracing::info!("took over {} listening sockets", received.len());
                self.fds.fds().handed_over.extend(received);
            }
            Err(err) => tracing::error!("upgrading without the previous listening sockets: {err}"),
        }
    }

    /// Adds a service, started when the server runs
    pub fn add_service(
        &mut self,
        service: impl crate::services::ServiceWithDependents,
    ) -> ServiceHandle {
        self.add_boxed_service(Box::new(service))
    }

    pub fn add_services(&mut self, services: Vec<BoxedService>) -> Vec<ServiceHandle> {
        services
            .into_iter()
            .map(|service| self.add_boxed_service(service))
            .collect()
    }

    fn add_boxed_service(&mut self, service: BoxedService) -> ServiceHandle {
        let (ready, receiver) = watch::channel(false);
        let handle = ServiceHandle {
            id: self.services.len(),
            name: service.service_name(),
            ready: receiver,
            dependencies: Arc::default(),
        };
        self.services.push(Pending {
            service,
            handle: handle.clone(),
            ready,
        });
        handle
    }

    /// Runs the services until a signal stops the server
    pub fn run_forever(self) -> ! {
        let Server {
            configuration,
            services,
            fds,
            shutdown,
            ..
        } = self;
        close_undeclared(&services, &fds);
        for pending in services {
            start(pending, &configuration, &fds, shutdown.subscribe());
        }
        let code = match signals(&configuration, &fds, &shutdown) {
            Ok(code) => code,
            Err(err) => {
                tracing::error!("can't wait for signals: {err}");
                1
            }
        };
        std::process::exit(code);
    }
}

/// Closes the sockets handed over that no service listens on anymore (e.g. a `tcp` block
/// removed before the upgrade)
fn close_undeclared(services: &[Pending], fds: &ListenFds) {
    let declared = services
        .iter()
        .filter_map(|pending| pending.service.service_listen_addresses())
        .flatten()
        .collect::<std::collections::HashSet<_>>();
    fds.fds().handed_over.retain(|address, _| {
        let kept = declared.contains(address);
        if !kept {
            tracing::info!("closing {address}, which nothing listens on anymore");
        }
        kept
    });
}

/// Starts `pending` on threads of its own, once its dependencies are ready
fn start(pending: Pending, configuration: &ServerConf, fds: &ListenFds, shutdown: ShutdownWatch) {
    let Pending {
        service,
        handle,
        ready,
    } = pending;
    let threads = service
        .service_threads()
        .unwrap_or(configuration.threads)
        .max(1);
    let fds = fds.clone();
    let name = handle.name.clone();
    let dependencies = handle
        .dependencies
        .lock()
        .map(|dependencies| dependencies.clone())
        .unwrap_or_default();
    // Without work stealing: connections are spread over threads of their own, each keeping its
    // connections (Pingora's)
    let pinned = (!configuration.work_stealing && threads > 1)
        .then(|| workers(threads, &name))
        .flatten();
    let threads = if pinned.is_some() { 1 } else { threads };
    let spawned = std::thread::Builder::new()
        .name(name.clone())
        .spawn(move || {
            let runtime = match runtime(threads, &name) {
                Ok(runtime) => runtime,
                Err(err) => {
                    tracing::error!("can't start the threads of {name}: {err}");
                    return;
                }
            };
            let serving = async move {
                for mut dependency in dependencies {
                    let _ = dependency.wait_for(|ready| *ready).await;
                }
                run(service, fds, shutdown, ServiceReadyNotifier::new(ready)).await;
                // What it started (e.g. connections finishing after it stopped accepting) runs
                // until the server exits
                std::future::pending::<()>().await;
            };
            match pinned {
                Some(workers) => runtime.block_on(WORKERS.scope(workers, serving)),
                None => runtime.block_on(serving),
            }
        });
    if let Err(err) = spawned {
        tracing::error!("can't start {}: {err}", handle.name);
    }
}

async fn run(
    service: Box<dyn AnyService>,
    fds: ListenFds,
    shutdown: ShutdownWatch,
    ready: ServiceReadyNotifier,
) {
    service.start(Some(fds), shutdown, ready).await;
}

tokio::task_local! {
    /// The threads of the service running, when they don't steal work from each other
    static WORKERS: Arc<Workers>;
}

/// Threads each running tasks of their own, given in turn
pub struct Workers {
    handles: Vec<tokio::runtime::Handle>,
    next: std::sync::atomic::AtomicUsize,
}

impl Workers {
    fn next(&self) -> Option<&tokio::runtime::Handle> {
        let turn = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.handles.get(turn % self.handles.len().max(1))
    }
}

/// Runs `task` (e.g. a connection) on the service's threads: when they don't steal work from
/// each other, on the next one in turn, where it stays
pub fn spawn<F>(task: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let handle = WORKERS
        .try_with(|workers| workers.next().cloned())
        .ok()
        .flatten();
    match handle {
        Some(handle) => drop(handle.spawn(task)),
        None => drop(tokio::spawn(task)),
    }
}

/// `threads` threads, each with a runtime of its own (running until the server exits)
fn workers(threads: usize, name: &str) -> Option<Arc<Workers>> {
    let handles = (0..threads)
        .map(|index| worker(&format!("{name}-{index}")))
        .collect::<std::io::Result<Vec<_>>>();
    match handles {
        Ok(handles) => Some(Arc::new(Workers {
            handles,
            next: std::sync::atomic::AtomicUsize::new(0),
        })),
        Err(err) => {
            tracing::error!("can't start the threads of {name}: {err}");
            None
        }
    }
}

fn worker(name: &str) -> std::io::Result<tokio::runtime::Handle> {
    let runtime = runtime(1, name)?;
    let handle = runtime.handle().clone();
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || runtime.block_on(std::future::pending::<()>()))?;
    Ok(handle)
}

fn runtime(threads: usize, name: &str) -> std::io::Result<tokio::runtime::Runtime> {
    if threads == 1 {
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
    }
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .thread_name(name)
        .enable_all()
        .build()
}

/// Waits for the signal that stops the server; returns its exit code
fn signals(
    configuration: &ServerConf,
    fds: &ListenFds,
    shutdown: &watch::Sender<bool>,
) -> std::io::Result<i32> {
    use tokio::signal::unix::{signal, SignalKind};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let mut terminate = signal(SignalKind::terminate())?;
        let mut quit = signal(SignalKind::quit())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        tokio::select! {
            _ = interrupt.recv() => {
                tracing::info!("SIGINT received: exiting");
                return Ok(0);
            }
            _ = terminate.recv() => tracing::info!("SIGTERM received: shutting down"),
            _ = quit.recv() => {
                tracing::info!("SIGQUIT received: handing the listening sockets over");
                let (path, listening) = (configuration.upgrade_sock.clone(), fds.listening());
                let sent = tokio::task::spawn_blocking(move || transfer::send(&path, &listening)).await;
                match sent {
                    Ok(Ok(())) => tracing::info!("listening sockets handed over"),
                    Ok(Err(err)) => tracing::error!("can't hand the listening sockets over: {err}"),
                    Err(err) => tracing::error!("can't hand the listening sockets over: {err}"),
                }
                // Both processes accept for a while: the new one may still be starting
                tokio::time::sleep(ACCEPTING_AFTER_UPGRADE).await;
            }
        }
        shutdown.send_replace(true);
        // The services stop accepting; what's in progress has the grace period to finish
        let grace = configuration.grace_period_seconds.unwrap_or(60)
            + configuration.graceful_shutdown_timeout_seconds.unwrap_or(5);
        tokio::time::sleep(Duration::from_secs(grace)).await;
        Ok(0)
    })
}

/// Runs in the background, writing the process id to `pid_file`
fn daemonize(pid_file: &str) {
    #[cfg(target_os = "linux")]
    if let Err(err) = nix::unistd::daemon(true, false) {
        tracing::error!("can't run in the background: {err}");
        return;
    }
    if let Err(err) = std::fs::write(pid_file, std::process::id().to_string()) {
        tracing::warn!("can't write the process id to {pid_file:?}: {err}");
    }
}
