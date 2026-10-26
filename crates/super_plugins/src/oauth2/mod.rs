//! `oauth2 { provider = "github", ... }`: signs users in with an `OAuth2` provider

use std::sync::LazyLock;

use anyhow::Result;
use super_core::proxy::Session;

use crate::{RequestCtx, RoutePlugin};

use crate::Middleware;

mod claims;
pub(crate) mod filter;
// New providers can be added here
mod github;
mod workos;
//
mod provider;
mod secure_cookie;
mod shared;

/// The flows of every location: its key encrypts their states
static FILTER: LazyLock<filter::Oauth2> = LazyLock::new(filter::Oauth2::new);

pub(crate) struct Oauth2 {
    plugin: RoutePlugin,
}

impl Oauth2 {
    pub(crate) fn new(plugin: &RoutePlugin) -> Result<Self, String> {
        let settings: super::settings::Oauth2 = super::settings::read("oauth2", plugin)?;
        // Validations as the plugin reads them: `type` (also written `key`) and `value`
        let mut plugin = plugin.clone();
        if let (Some(validations), Some(config)) = (settings.validations, plugin.config.as_mut()) {
            let validations = validations
                .iter()
                .map(|validation| {
                    serde_json::json!({ "type": validation.check.name(), "value": validation.value })
                })
                .collect();
            config.insert("validations".into(), serde_json::Value::Array(validations));
        }
        Ok(Self { plugin })
    }
}

impl Middleware for Oauth2 {
    async fn request(&self, session: &mut Session, ctx: &mut dyn RequestCtx) -> Result<bool> {
        FILTER.request_filter(session, ctx, &self.plugin).await
    }
}
