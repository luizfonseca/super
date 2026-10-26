//! Super's core, on hyper: what Proksi used of Pingora, with its request phases.
//!
//! - [`proxy`]: the phases of a request ([`proxy::ProxyHttp`]) and its session
//! - [`server`], [`services`], [`apps`]: the server, its services and what they serve
//! - [`upstreams`]: connections to upstreams, kept for the next requests
//! - [`lb`]: load balancers, their backends and health checks
//! - [`cache`]: responses served from a storage
//! - [`tls`]: OpenSSL, as the listeners use it

// A library whose getters are mostly called for their value; errors are boxed, as Pingora's,
// since they're moved around more than they're created
#![allow(clippy::must_use_candidate, clippy::unnecessary_box_returns)]

#[cfg(any(test, feature = "test-util"))]
pub mod allocations;
pub mod apps;
pub mod cache;
pub mod error;
pub mod hash;
pub mod http;
pub mod lb;
pub mod modules;
pub mod protocols;
pub mod proxy;
pub mod server;
pub mod services;
pub mod tls;
pub mod upstreams;

pub use error::{Error, ErrorSource, ErrorType, OrErr, Result, RetryType};

// The tests count allocations (see `allocations`)
#[cfg(test)]
#[global_allocator]
static GLOBAL: allocations::Counting = allocations::Counting;
