//! Load balancers: the backends of an upstream, as discovered, whether each is healthy, and
//! which one a request goes to (as Pingora's)

use std::{
    collections::BTreeSet,
    hash::{Hash, Hasher},
    net::ToSocketAddrs,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use arc_swap::ArcSwap;
use http::Extensions;

use crate::{hash::PrehashedMap, protocols::l4::socket::SocketAddr, Result};

pub mod discovery;
pub mod health_check;
pub mod selection;

use discovery::ServiceDiscovery;
use health_check::HealthCheck;
use selection::BackendSelection;

/// An address to send requests or connections to
#[derive(Debug, Clone)]
pub struct Backend {
    pub addr: SocketAddr,
    /// Its share of the requests, among the backends of its load balancer
    pub weight: usize,
    /// What the discovery attached to it
    pub ext: Extensions,
}

impl Backend {
    /// # Errors
    ///
    /// When `addr` isn't an IP address and a port.
    pub fn new(addr: &str) -> Result<Self> {
        Self::new_with_weight(addr, 1)
    }

    /// # Errors
    ///
    /// When `addr` isn't an IP address and a port.
    pub fn new_with_weight(addr: &str, weight: usize) -> Result<Self> {
        let addr = addr.parse::<std::net::SocketAddr>().map_err(|err| {
            crate::Error::because(
                crate::ErrorType::InternalError,
                "invalid backend address",
                err,
            )
        })?;
        Ok(Self {
            addr: SocketAddr::Inet(addr),
            weight,
            ext: Extensions::new(),
        })
    }

    /// The key of its health
    pub fn hash_key(&self) -> u64 {
        crate::hash::hash_one(self)
    }
}

// Backends are the same with the same address and weight, whatever is attached to them
impl PartialEq for Backend {
    fn eq(&self, other: &Self) -> bool {
        self.addr == other.addr && self.weight == other.weight
    }
}

impl Eq for Backend {}

impl PartialOrd for Backend {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Backend {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (&self.addr, self.weight).cmp(&(&other.addr, other.weight))
    }
}

impl Hash for Backend {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.addr.hash(state);
        self.weight.hash(state);
    }
}

/// The health of a backend: healthy after enough checks in a row succeed, unhealthy after
/// enough fail. A backend can also be turned off (`enabled`).
#[derive(Debug)]
struct Health {
    healthy: AtomicBool,
    enabled: AtomicBool,
    /// Checks in a row that disagree with `healthy`
    counter: AtomicUsize,
}

impl Health {
    fn new() -> Self {
        Self {
            healthy: AtomicBool::new(true),
            enabled: AtomicBool::new(true),
            counter: AtomicUsize::new(0),
        }
    }

    fn ready(&self) -> bool {
        self.healthy.load(Ordering::Relaxed) && self.enabled.load(Ordering::Relaxed)
    }

    /// Records a check. Returns whether the backend's health changed.
    fn observe(&self, healthy: bool, threshold: usize) -> bool {
        if self.healthy.load(Ordering::Relaxed) == healthy {
            self.counter.store(0, Ordering::Relaxed);
            return false;
        }
        let in_a_row = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        if in_a_row < threshold.max(1) {
            return false;
        }
        self.counter.store(0, Ordering::Relaxed);
        self.healthy.store(healthy, Ordering::Relaxed);
        true
    }
}

/// The backends of a load balancer, as its discovery finds them, with their health
pub struct Backends {
    discovery: Box<dyn ServiceDiscovery + Send + Sync>,
    health_check: Option<Arc<dyn HealthCheck + Send + Sync>>,
    found: ArcSwap<BTreeSet<Backend>>,
    health: ArcSwap<PrehashedMap<Arc<Health>>>,
}

impl Backends {
    pub fn new(discovery: Box<dyn ServiceDiscovery + Send + Sync>) -> Self {
        Self {
            discovery,
            health_check: None,
            found: ArcSwap::default(),
            health: ArcSwap::default(),
        }
    }

    pub fn set_health_check(&mut self, health_check: Box<dyn HealthCheck + Send + Sync>) {
        self.health_check = Some(health_check.into());
    }

    /// The backends found last
    pub fn get_backend(&self) -> Arc<BTreeSet<Backend>> {
        self.found.load_full()
    }

    /// Whether `backend` is healthy and enabled (backends never checked are)
    pub fn ready(&self, backend: &Backend) -> bool {
        self.health
            .load()
            .get(&backend.hash_key())
            .is_none_or(|health| health.ready())
    }

    /// Turns `backend` on or off, whatever its health
    pub fn set_enable(&self, backend: &Backend, enabled: bool) {
        if let Some(health) = self.health.load().get(&backend.hash_key()) {
            health.enabled.store(enabled, Ordering::Relaxed);
        }
    }

    /// Discovers the backends again. `changed` is called with the new ones when they changed.
    ///
    /// # Errors
    ///
    /// When the discovery fails.
    pub async fn update(&self, changed: impl FnOnce(&BTreeSet<Backend>)) -> Result<()> {
        let (backends, enablement) = self.discovery.discover().await?;
        if **self.found.load() != backends {
            // Backends already known keep their health
            let previous = self.health.load();
            let health = backends
                .iter()
                .map(|backend| {
                    let key = backend.hash_key();
                    let health = previous
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(|| Arc::new(Health::new()));
                    (key, health)
                })
                .collect::<PrehashedMap<_>>();
            changed(&backends);
            self.health.store(Arc::new(health));
            self.found.store(Arc::new(backends));
        }
        let health = self.health.load();
        for (key, enabled) in enablement {
            if let Some(health) = health.get(&key) {
                health.enabled.store(enabled, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    /// Checks every backend, `parallel` or one after the other
    pub async fn run_health_check(&self, parallel: bool) {
        let Some(health_check) = self.health_check.clone() else {
            return;
        };
        let backends = self.get_backend();
        let health = self.health.load_full();
        let checks = backends
            .iter()
            .filter_map(|backend| Some((backend, health.get(&backend.hash_key())?.clone())))
            .map(|(backend, health)| check_one(health_check.as_ref(), backend, health));
        if parallel {
            futures_util::future::join_all(checks).await;
            return;
        }
        for check in checks {
            check.await;
        }
    }
}

async fn check_one(
    health_check: &(dyn HealthCheck + Send + Sync),
    backend: &Backend,
    health: Arc<Health>,
) {
    let result = health_check.check(backend).await;
    let healthy = result.is_ok();
    if let Err(err) = result {
        tracing::debug!("health check of {} failed: {err}", backend.addr);
    }
    if health.observe(healthy, health_check.health_threshold(healthy)) {
        tracing::info!(
            "{} is now {}",
            backend.addr,
            if healthy { "healthy" } else { "unhealthy" }
        );
        health_check.health_status_change(backend, healthy).await;
    }
}

/// Backends and how one is chosen among them
pub struct LoadBalancer<S> {
    backends: Backends,
    selector: ArcSwap<S>,
    /// How often the backends are checked (by whoever runs the checks), if they are
    pub health_check_frequency: Option<Duration>,
    /// How often they're discovered again, if they are
    pub update_frequency: Option<Duration>,
    pub parallel_health_check: bool,
}

impl<S: BackendSelection> LoadBalancer<S> {
    pub fn from_backends(backends: Backends) -> Self {
        let selector = S::build(&backends.get_backend());
        Self {
            backends,
            selector: ArcSwap::new(Arc::new(selector)),
            health_check_frequency: None,
            update_frequency: None,
            parallel_health_check: false,
        }
    }

    /// A load balancer of fixed addresses
    ///
    /// # Errors
    ///
    /// When an address can't be resolved.
    pub fn try_from_iter<A: ToSocketAddrs, T: IntoIterator<Item = A>>(
        iter: T,
    ) -> std::io::Result<Self> {
        let discovery = discovery::Static::try_from_iter(iter)?;
        let backends = Backends::new(discovery);
        let load_balancer = Self::from_backends(backends);
        // A static discovery answers right away
        let _ = futures_util::FutureExt::now_or_never(load_balancer.update());
        Ok(load_balancer)
    }

    /// Discovers the backends again
    ///
    /// # Errors
    ///
    /// When the discovery fails.
    pub async fn update(&self) -> Result<()> {
        self.backends
            .update(|backends| self.selector.store(Arc::new(S::build(backends))))
            .await
    }

    /// A healthy backend for `key`, among the next `max_iterations`
    pub fn select(&self, key: &[u8], max_iterations: usize) -> Option<Backend> {
        self.select_with(key, max_iterations, |_, healthy| healthy)
    }

    /// The first backend for `key` that `accept` takes (given whether it's healthy), among the
    /// next `max_iterations`
    pub fn select_with(
        &self,
        key: &[u8],
        max_iterations: usize,
        accept: impl Fn(&Backend, bool) -> bool,
    ) -> Option<Backend> {
        let selector = self.selector.load();
        let selected = selector
            .iter(key)
            .take(max_iterations)
            .find(|backend| accept(backend, self.backends.ready(backend)))
            .cloned();
        selected
    }

    pub fn set_health_check(&mut self, health_check: Box<dyn HealthCheck + Send + Sync>) {
        self.backends.set_health_check(health_check);
    }

    pub fn backends(&self) -> &Backends {
        &self.backends
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{selection::RoundRobin, Backend, LoadBalancer};

    #[test]
    fn test_backends_take_turns() {
        let load_balancer =
            LoadBalancer::<RoundRobin>::try_from_iter(["10.0.0.1:80", "10.0.0.2:80"]).unwrap();
        let picked = (0..4)
            .map(|_| load_balancer.select(b"", 8).unwrap().addr.to_string())
            .collect::<Vec<_>>();
        assert_eq!(picked[0], picked[2]);
        assert_eq!(picked[1], picked[3]);
        assert_ne!(picked[0], picked[1]);
    }

    #[test]
    fn test_unhealthy_backends_are_skipped() {
        let load_balancer =
            LoadBalancer::<RoundRobin>::try_from_iter(["10.0.0.1:80", "10.0.0.2:80"]).unwrap();
        let down = Backend::new("10.0.0.1:80").unwrap();
        load_balancer.backends().set_enable(&down, false);
        assert!(!load_balancer.backends().ready(&down));

        for _ in 0..4 {
            assert_eq!(
                load_balancer.select(b"", 8).unwrap().addr.to_string(),
                "10.0.0.2:80"
            );
        }
        // Unless every backend is accepted
        let any = (0..4)
            .filter_map(|_| load_balancer.select_with(b"", 8, |_, _| true))
            .any(|backend| backend == down);
        assert!(any);
    }

    #[test]
    fn test_health_flips_after_checks_in_a_row() {
        let health = super::Health::new();
        assert!(!health.observe(false, 2));
        assert!(health.ready());
        assert!(health.observe(false, 2));
        assert!(!health.ready());
        // A success in between starts over
        assert!(!health.observe(true, 2));
        assert!(!health.observe(false, 2));
        assert!(!health.observe(true, 2));
        assert!(health.observe(true, 2));
        assert!(health.ready());
    }

    #[test]
    fn test_reading_health_allocates_nothing() {
        let load_balancer = Arc::new(
            LoadBalancer::<RoundRobin>::try_from_iter(["10.0.0.1:80", "10.0.0.2:80"]).unwrap(),
        );
        let backends = load_balancer.backends();
        crate::allocations::assert_no_allocations("reading backends and their health", || {
            let all = backends.get_backend();
            all.iter().filter(|backend| backends.ready(backend)).count()
        });
    }
}
