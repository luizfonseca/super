//! Super's core, on hyper: what Proksi used of Pingora, with its request phases.
//!
//! - [`proxy`]: the phases of a request ([`proxy::ProxyHttp`]) and its session
//! - [`server`], [`services`], [`apps`]: the server, its services and what they serve
//! - [`upstreams`]: connections to upstreams, kept for the next requests
//! - [`lb`]: load balancers, their backends and health checks
//! - [`cache`]: responses served from a storage
//! - [`tls`]: OpenSSL, as the listeners use it

pub mod error;
pub mod http;

pub use error::{Error, ErrorSource, ErrorType, OrErr, Result, RetryType};
