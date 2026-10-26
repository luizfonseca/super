//! `basic_auth { user = "...", pass = "..." }`: answers `401` without valid credentials

use anyhow::Result;
use super_core::proxy::Session;

use crate::{RequestCtx, RoutePlugin};

use crate::Middleware;

pub(crate) mod filter;

pub(crate) struct BasicAuth {
    plugin: RoutePlugin,
}

impl BasicAuth {
    pub(crate) fn new(plugin: &RoutePlugin) -> Result<Self, String> {
        let _: super::settings::BasicAuth = super::settings::read("basic_auth", plugin)?;
        Ok(Self {
            plugin: plugin.clone(),
        })
    }
}

impl Middleware for BasicAuth {
    async fn request(&self, session: &mut Session, ctx: &mut dyn RequestCtx) -> Result<bool> {
        filter::BasicAuth
            .request_filter(session, ctx, &self.plugin)
            .await
    }
}
