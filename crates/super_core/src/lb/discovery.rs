//! How a load balancer finds its backends

use std::{
    collections::{BTreeSet, HashMap},
    net::ToSocketAddrs,
};

use arc_swap::ArcSwap;
use async_trait::async_trait;

use super::Backend;
use crate::{protocols::l4::socket::SocketAddr, Result};

/// Finds the backends of a load balancer: each one, and which are turned on or off (by
/// [`Backend::hash_key`])
#[async_trait]
pub trait ServiceDiscovery {
    async fn discover(&self) -> Result<(BTreeSet<Backend>, HashMap<u64, bool>)>;
}

/// Backends that only change when they're set
#[derive(Default)]
pub struct Static {
    backends: ArcSwap<BTreeSet<Backend>>,
}

impl Static {
    pub fn new(backends: BTreeSet<Backend>) -> Box<Self> {
        Box::new(Self {
            backends: ArcSwap::new(backends.into()),
        })
    }

    /// The addresses of `iter`, resolved now
    ///
    /// # Errors
    ///
    /// When an address can't be resolved.
    pub fn try_from_iter<A: ToSocketAddrs, T: IntoIterator<Item = A>>(
        iter: T,
    ) -> std::io::Result<Box<Self>> {
        let mut backends = BTreeSet::new();
        for addresses in iter {
            for addr in addresses.to_socket_addrs()? {
                backends.insert(Backend {
                    addr: SocketAddr::Inet(addr),
                    weight: 1,
                    ext: http::Extensions::new(),
                });
            }
        }
        Ok(Self::new(backends))
    }

    pub fn set(&self, backends: BTreeSet<Backend>) {
        self.backends.store(backends.into());
    }

    pub fn get(&self) -> BTreeSet<Backend> {
        (**self.backends.load()).clone()
    }
}

#[async_trait]
impl ServiceDiscovery for Static {
    async fn discover(&self) -> Result<(BTreeSet<Backend>, HashMap<u64, bool>)> {
        Ok((self.get(), HashMap::new()))
    }
}
