//! What every response of a proxy goes through: its compression

use std::any::Any;

pub mod compression;

/// Pingora's paths
pub mod http {
    pub use super::compression;
}

/// The modules of a request
#[derive(Default)]
pub struct ModulesCtx {
    compression: compression::ResponseCompression,
}

impl ModulesCtx {
    /// The module of type `T`
    pub fn get_mut<T: Any>(&mut self) -> Option<&mut T> {
        (&mut self.compression as &mut dyn Any).downcast_mut()
    }

    pub fn compression(&mut self) -> &mut compression::ResponseCompression {
        &mut self.compression
    }
}
