//! Health checks of backends

use std::time::Duration;

use async_trait::async_trait;
use tokio::net::TcpStream;

use super::Backend;
use crate::{Error, ErrorType, Result};

/// Told when a backend's health changes
#[async_trait]
pub trait HealthObserve {
    async fn observe(&self, target: &Backend, healthy: bool);
}

/// A check of a backend
#[async_trait]
pub trait HealthCheck {
    /// # Errors
    ///
    /// When the backend isn't healthy.
    async fn check(&self, target: &Backend) -> Result<()>;

    /// Called when the backend becomes healthy, or unhealthy
    async fn health_status_change(&self, _target: &Backend, _healthy: bool) {}

    /// How many checks in a row it takes to become `healthy` (or unhealthy)
    fn health_threshold(&self, healthy: bool) -> usize;
}

/// Healthy when a TCP connection can be made
pub struct TcpHealthCheck {
    pub consecutive_success: usize,
    pub consecutive_failure: usize,
    pub connect_timeout: Duration,
    pub health_changed_callback: Option<Box<dyn HealthObserve + Send + Sync>>,
}

impl TcpHealthCheck {
    pub fn new() -> Box<Self> {
        Box::new(Self {
            consecutive_success: 1,
            consecutive_failure: 1,
            connect_timeout: Duration::from_secs(1),
            health_changed_callback: None,
        })
    }
}

#[async_trait]
impl HealthCheck for TcpHealthCheck {
    async fn check(&self, target: &Backend) -> Result<()> {
        let Some(addr) = target.addr.as_inet() else {
            return Err(Error::explain(ErrorType::ConnectError, "not an IP address"));
        };
        match tokio::time::timeout(self.connect_timeout, TcpStream::connect(addr)).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(err)) => Err(Error::because(ErrorType::ConnectError, "health check", err)),
            Err(_) => Err(Error::explain(ErrorType::ConnectTimedout, "health check")),
        }
    }

    async fn health_status_change(&self, target: &Backend, healthy: bool) {
        if let Some(callback) = &self.health_changed_callback {
            callback.observe(target, healthy).await;
        }
    }

    fn health_threshold(&self, healthy: bool) -> usize {
        if healthy {
            self.consecutive_success
        } else {
            self.consecutive_failure
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use async_trait::async_trait;

    use super::{HealthObserve, TcpHealthCheck};
    use crate::lb::{selection::RoundRobin, Backend, LoadBalancer};

    struct Changes(Arc<AtomicUsize>);

    #[async_trait]
    impl HealthObserve for Changes {
        async fn observe(&self, _target: &Backend, _healthy: bool) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[tokio::test]
    async fn test_backends_that_dont_accept_become_unhealthy() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up = listener.local_addr().unwrap().to_string();
        // Bound, then closed: nothing accepts there
        let down = {
            let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            closed.local_addr().unwrap().to_string()
        };
        let mut load_balancer = LoadBalancer::<RoundRobin>::try_from_iter([&up, &down]).unwrap();
        let changes = Arc::new(AtomicUsize::new(0));
        let mut check = TcpHealthCheck::new();
        check.consecutive_failure = 2;
        check.health_changed_callback = Some(Box::new(Changes(changes.clone())));
        load_balancer.set_health_check(check);

        let backends = load_balancer.backends();
        backends.run_health_check(true).await;
        assert!(backends.ready(&Backend::new(&down).unwrap()));
        backends.run_health_check(false).await;
        assert!(!backends.ready(&Backend::new(&down).unwrap()));
        assert!(backends.ready(&Backend::new(&up).unwrap()));
        assert_eq!(changes.load(Ordering::Relaxed), 1);
    }
}
