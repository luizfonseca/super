//! HTTP/1.1 and HTTP/2 with clients, on hyper

pub mod body;
pub mod server;

pub use body::ResponseBody;
pub use server::{Connection, ResponseWritten, ServerSession};

/// Pingora's path of the compression algorithms
pub mod compression {
    pub use crate::modules::compression::Algorithm;
}
