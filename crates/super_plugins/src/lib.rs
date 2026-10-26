//! Super's built-in plugins (Proksi's): the building blocks of a location besides `headers` and `cache`
//! (see ADR 0003), such as `basic_auth`, `rate_limit` or `static_files`.
//!
//! Each plugin is built once per location from its [`RoutePlugin`], validated when the
//! configuration loads, and takes part in the stages of a request it needs ([`Middleware`]). What
//! a plugin needs from Super for a request (its host, its client, applying the location's
//! response blocks to an answer) it asks through [`RequestCtx`].

use std::{
    any::Any, borrow::Cow, collections::HashMap, future::Future, net::IpAddr, pin::Pin, sync::Arc,
    time::Duration,
};

use anyhow::Result;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use super_core::{
    http::{RequestHeader, ResponseHeader},
    proxy::Session,
};

use bandwidth_limit::BandwidthLimit;
use basic_auth::BasicAuth;
use body_limit::BodyLimit;
use body_rewrite::BodyRewrite;
use compress::Compress;
use cors::Cors;
use forward_auth::ForwardAuth;
use ip_filter::IpFilter;
use jwt::Jwt;
use oauth2::Oauth2;
use openapi::OpenApi;
use rate_limit::RateLimit;
use redirect::{Redirect, Respond};
use request_id::RequestId;
use static_files::StaticFiles;
use waiting_room::WaitingRoom;

#[cfg(any(test, feature = "test-util"))]
pub mod allocations;
mod bandwidth_limit;
mod basic_auth;
pub mod body_limit;
mod body_rewrite;
mod compress;
mod cors;
mod filter;
mod forward_auth;
mod ip_filter;
pub mod ip_set;
pub mod jwt;
mod oauth2;
pub mod openapi;
mod priority;
pub mod rate_limit;
mod redirect;
mod request_id;
pub mod respond;
pub mod settings;
mod static_files;
mod waiting_room;

pub use priority::{plugin_priority, priority};

// The tests count allocations (see `allocations`)
#[cfg(test)]
#[global_allocator]
static GLOBAL: allocations::Counting = allocations::Counting;

/// A plugin of a location, as configured: its name, and its settings
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, schemars::JsonSchema)]
pub struct RoutePlugin {
    /// The name of the plugin (must be a valid plugin name)
    pub name: Cow<'static, str>,

    /// The configuration for the plugin - we are not enforcing a specific format.
    /// Each plugin is in charge of parsing the configuration.
    /// The configuration is a key-value pair where the key is a string and
    /// the value is a JSON object (ex: `{ "key": "value" }`)
    pub config: Option<HashMap<Cow<'static, str>, serde_json::Value>>,
}

/// What a plugin can ask of Super about the request it's handling
pub trait RequestCtx: Send {
    /// The host the request is for, without its port
    fn host(&self) -> &str;

    /// The request's ID, once `request_id` gave it one
    fn request_id(&self) -> Option<&http::HeaderValue>;

    fn set_request_id(&mut self, id: http::HeaderValue);

    /// Counts `bytes` more of the request's body, and returns how many were read so far
    fn count_request_body(&mut self, bytes: u64) -> u64;

    /// The client, behind trusted proxies the one they forwarded
    fn client(&self, session: &Session) -> Option<IpAddr>;

    /// Whether `peer` is a trusted proxy, whose forwarded headers are kept
    fn is_trusted(&self, peer: IpAddr) -> bool;

    /// `path` as locations read it (`//a/./b` is `/a/b`)
    fn normalize_path<'a>(&self, path: &'a str) -> Cow<'a, str>;

    /// Changes `response`, an answer of Super itself, as the location's response blocks and the
    /// rules change upstreams' responses (its `headers`, its CORS headers...). Boxed: blocks answer
    /// through it, and it runs blocks (only answers of Super itself take it).
    fn response_blocks<'a>(
        &'a mut self,
        session: &'a mut Session,
        response: &'a mut ResponseHeader,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

    /// Adds a header to the response, whoever answers the request
    fn add_response_header(&mut self, name: http::HeaderName, value: http::HeaderValue);

    /// What blocks keep for this request, by block: see [`block_state`]
    fn block_states(&mut self) -> &mut Vec<(usize, Box<dyn Any + Send + Sync>)>;

    /// The store shared by the replicas (memory, Redis or SQL)
    fn store(&self) -> Arc<dyn super_store::Store>;

    /// How long to wait before sending `body`, part of an answer of Super itself, as the
    /// location's blocks and the rules pace upstreams' responses (`bandwidth_limit`)
    fn response_body_delay(&mut self, session: &Session, body: Option<&Bytes>) -> Option<Duration>;
}

/// The stages of a request a block can take part in
pub trait Middleware: Send + Sync {
    /// Before the request is proxied. `true` when the block answered the request, which ends it.
    fn request(
        &self,
        _session: &mut Session,
        _ctx: &mut dyn RequestCtx,
    ) -> impl Future<Output = Result<bool>> + Send {
        async { Ok(false) }
    }

    /// Each part of the request body, as it's proxied, which a block can change (the last one
    /// with `end_of_stream`).
    ///
    /// # Errors
    ///
    /// An error ends the request.
    fn request_body(
        &self,
        _body: &mut Option<Bytes>,
        _end_of_stream: bool,
        _ctx: &mut dyn RequestCtx,
    ) -> super_core::Result<()> {
        Ok(())
    }

    /// Changes the request sent to the upstream
    fn upstream_request(
        &self,
        _session: &mut Session,
        _request: &mut RequestHeader,
        _ctx: &mut dyn RequestCtx,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }

    /// Changes the response of the upstream, before it's cached
    ///
    /// # Errors
    ///
    /// An error is logged, and the response goes on unchanged by this block.
    fn upstream_response(
        &self,
        _session: &mut Session,
        _response: &mut ResponseHeader,
        _ctx: &mut dyn RequestCtx,
    ) -> Result<()> {
        Ok(())
    }

    /// Each part of the response body sent to the client (including responses served from the
    /// cache): how long to wait before sending it
    fn response_body(
        &self,
        _session: &Session,
        _body: Option<&Bytes>,
        _ctx: &mut dyn RequestCtx,
    ) -> Option<Duration> {
        None
    }

    /// Changes the response sent to the client (including responses served from the cache)
    fn response(
        &self,
        _session: &mut Session,
        _response: &mut ResponseHeader,
        _ctx: &mut dyn RequestCtx,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }
}

/// The kinds of blocks, the plugins of this crate and Super's own `headers` and `cache`. Blocks
/// with the same priority run in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    RequestId,
    IpFilter,
    Cors,
    RateLimit,
    BandwidthLimit,
    WaitingRoom,
    Redirect,
    Respond,
    BodyLimit,
    BasicAuth,
    Jwt,
    Oauth2,
    ForwardAuth,
    BodyRewrite,
    OpenApi,
    Compress,
    Headers,
    Cache,
    StaticFiles,
}

impl Kind {
    /// Every kind, in the order blocks with the same priority run
    pub const ALL: [Kind; 19] = [
        Kind::RequestId,
        Kind::IpFilter,
        Kind::Cors,
        Kind::RateLimit,
        Kind::BandwidthLimit,
        Kind::WaitingRoom,
        Kind::Redirect,
        Kind::Respond,
        Kind::BodyLimit,
        Kind::BasicAuth,
        Kind::Jwt,
        Kind::Oauth2,
        Kind::ForwardAuth,
        Kind::BodyRewrite,
        Kind::OpenApi,
        Kind::Compress,
        Kind::Headers,
        Kind::Cache,
        Kind::StaticFiles,
    ];

    /// The kind named `name`, as written in the configuration
    #[must_use]
    pub fn from_name(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.name() == name)
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Kind::RequestId => "request_id",
            Kind::IpFilter => "ip_filter",
            Kind::Cors => "cors",
            Kind::RateLimit => "rate_limit",
            Kind::BandwidthLimit => "bandwidth_limit",
            Kind::WaitingRoom => "waiting_room",
            Kind::Redirect => "redirect",
            Kind::Respond => "respond",
            Kind::BodyLimit => "body_limit",
            Kind::BasicAuth => "basic_auth",
            Kind::Jwt => "jwt",
            Kind::Oauth2 => "oauth2",
            Kind::ForwardAuth => "forward_auth",
            Kind::BodyRewrite => "body_rewrite",
            Kind::OpenApi => "openapi",
            Kind::Compress => "compress",
            Kind::Headers => "headers",
            Kind::Cache => "cache",
            Kind::StaticFiles => "static_files",
        }
    }

    /// Where the block runs unless it sets `priority`
    #[must_use]
    pub fn default_priority(self) -> u8 {
        match self {
            Kind::RequestId
            | Kind::IpFilter
            | Kind::Cors
            | Kind::RateLimit
            | Kind::BandwidthLimit
            | Kind::WaitingRoom => 0,
            Kind::Redirect | Kind::Respond => 5,
            Kind::BodyLimit => 10,
            Kind::BasicAuth | Kind::Jwt | Kind::Oauth2 | Kind::ForwardAuth => 20,
            // After authentication: bodies of requests that are refused aren't read
            Kind::BodyRewrite => 25,
            // After authentication: who isn't let in learns nothing of the API
            Kind::OpenApi => 22,
            Kind::Compress => 40,
            Kind::Headers => 50,
            Kind::Cache => 60,
            // Last: the other blocks see the request before it's answered
            Kind::StaticFiles => 90,
        }
    }

    /// Whether the block can answer a request instead of the upstream
    #[must_use]
    pub fn answers(self) -> bool {
        matches!(
            self,
            Kind::IpFilter
                | Kind::RateLimit
                | Kind::WaitingRoom
                | Kind::BodyLimit
                | Kind::BasicAuth
                | Kind::Jwt
                | Kind::Oauth2
                | Kind::ForwardAuth
                | Kind::BodyRewrite
                | Kind::OpenApi
        )
    }
}

/// A plugin of a location
pub struct Plugin {
    pub kind: Kind,
    pub priority: u8,
    pub middleware: AnyPlugin,
}

/// Any plugin of this crate: its stages are this crate's, called without boxing them
#[derive(Clone)]
pub struct AnyPlugin(Arc<Built>);

enum Built {
    RequestId(RequestId),
    IpFilter(IpFilter),
    Cors(Cors),
    RateLimit(RateLimit),
    BandwidthLimit(BandwidthLimit),
    WaitingRoom(WaitingRoom),
    BodyLimit(BodyLimit),
    Redirect(Redirect),
    Respond(Respond),
    BasicAuth(BasicAuth),
    Jwt(Jwt),
    Oauth2(Oauth2),
    ForwardAuth(ForwardAuth),
    BodyRewrite(BodyRewrite),
    OpenApi(OpenApi),
    Compress(Compress),
    StaticFiles(StaticFiles),
}

/// `$call` with `$plugin`, whichever plugin `$built` is
macro_rules! each {
    ($built:expr, $plugin:ident => $call:expr) => {
        match $built {
            Built::RequestId($plugin) => $call,
            Built::IpFilter($plugin) => $call,
            Built::Cors($plugin) => $call,
            Built::RateLimit($plugin) => $call,
            Built::BandwidthLimit($plugin) => $call,
            Built::WaitingRoom($plugin) => $call,
            Built::BodyLimit($plugin) => $call,
            Built::Redirect($plugin) => $call,
            Built::Respond($plugin) => $call,
            Built::BasicAuth($plugin) => $call,
            Built::Jwt($plugin) => $call,
            Built::Oauth2($plugin) => $call,
            Built::ForwardAuth($plugin) => $call,
            Built::BodyRewrite($plugin) => $call,
            Built::OpenApi($plugin) => $call,
            Built::Compress($plugin) => $call,
            Built::StaticFiles($plugin) => $call,
        }
    };
}

impl Middleware for AnyPlugin {
    async fn request(&self, session: &mut Session, ctx: &mut dyn RequestCtx) -> Result<bool> {
        each!(&*self.0, plugin => plugin.request(session, ctx).await)
    }

    fn request_body(
        &self,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut dyn RequestCtx,
    ) -> super_core::Result<()> {
        each!(&*self.0, plugin => plugin.request_body(body, end_of_stream, ctx))
    }

    async fn upstream_request(
        &self,
        session: &mut Session,
        request: &mut RequestHeader,
        ctx: &mut dyn RequestCtx,
    ) -> Result<()> {
        each!(&*self.0, plugin => plugin.upstream_request(session, request, ctx).await)
    }

    fn upstream_response(
        &self,
        session: &mut Session,
        response: &mut ResponseHeader,
        ctx: &mut dyn RequestCtx,
    ) -> Result<()> {
        each!(&*self.0, plugin => plugin.upstream_response(session, response, ctx))
    }

    fn response_body(
        &self,
        session: &Session,
        body: Option<&Bytes>,
        ctx: &mut dyn RequestCtx,
    ) -> Option<Duration> {
        each!(&*self.0, plugin => plugin.response_body(session, body, ctx))
    }

    async fn response(
        &self,
        session: &mut Session,
        response: &mut ResponseHeader,
        ctx: &mut dyn RequestCtx,
    ) -> Result<()> {
        each!(&*self.0, plugin => plugin.response(session, response, ctx).await)
    }
}

/// The plugin `plugin` configures in the location at `location_path`, `None` for names that
/// aren't plugins, or why its configuration is invalid (with the plugin's name in the message)
#[must_use]
pub fn build(
    plugin: &RoutePlugin,
    location_path: &str,
    id: &str,
) -> Option<Result<Plugin, String>> {
    let built = |kind, middleware: Result<Built, String>| {
        Some(middleware.and_then(|middleware| {
            Ok(Plugin {
                kind,
                priority: plugin_priority(kind, plugin)?,
                middleware: AnyPlugin(Arc::new(middleware)),
            })
        }))
    };
    // Other plugins of `routes = [...]` and docker labels are ignored, as before
    let kind = Kind::from_name(&plugin.name)?;
    let middleware = match kind {
        Kind::RequestId => RequestId::new(plugin).map(Built::RequestId),
        Kind::IpFilter => IpFilter::new(plugin).map(Built::IpFilter),
        Kind::Cors => Cors::new(plugin).map(Built::Cors),
        Kind::RateLimit => RateLimit::new(plugin, id)
            .map_err(|err| err.to_string())
            .map(Built::RateLimit),
        Kind::BandwidthLimit => BandwidthLimit::new(plugin).map(Built::BandwidthLimit),
        Kind::WaitingRoom => WaitingRoom::new(plugin, location_path).map(Built::WaitingRoom),
        Kind::BodyLimit => BodyLimit::new(plugin).map(Built::BodyLimit),
        Kind::Redirect => Redirect::new(plugin).map(Built::Redirect),
        Kind::Respond => Respond::new(plugin).map(Built::Respond),
        Kind::BasicAuth => BasicAuth::new(plugin).map(Built::BasicAuth),
        Kind::Jwt => Jwt::new(plugin).map(Built::Jwt),
        Kind::Oauth2 => Oauth2::new(plugin).map(Built::Oauth2),
        Kind::ForwardAuth => ForwardAuth::new(plugin).map(Built::ForwardAuth),
        Kind::BodyRewrite => BodyRewrite::new(plugin).map(Built::BodyRewrite),
        Kind::OpenApi => OpenApi::new(plugin).map(Built::OpenApi),
        Kind::Compress => Compress::new(plugin).map(Built::Compress),
        Kind::StaticFiles => StaticFiles::new(plugin, location_path).map(Built::StaticFiles),
        // Super's own blocks, built by Super
        Kind::Headers | Kind::Cache => return None,
    };
    built(kind, middleware)
}

#[cfg(test)]
mod tests {
    use super::Kind;

    #[test]
    fn test_kinds_are_listed_once_in_order() {
        // In the order blocks with the same priority run: the order of the enum
        assert!(Kind::ALL.windows(2).all(|pair| pair[0] < pair[1]));
        for kind in Kind::ALL {
            assert_eq!(Kind::from_name(kind.name()), Some(kind));
        }
        assert_eq!(Kind::from_name("unknown"), None);
    }

    #[test]
    fn test_every_plugin_has_settings() {
        let schemas = crate::settings::schemas();
        for kind in Kind::ALL {
            let plugin = !matches!(kind, Kind::Headers | Kind::Cache);
            assert_eq!(schemas.contains_key(kind.name()), plugin, "{}", kind.name());
        }
        // And every set of settings is a plugin's (none missing from `Kind::ALL`)
        assert!(schemas.keys().all(|name| Kind::from_name(name).is_some()));
    }
}

/// What the block `owner` keeps for the current request, if it set something
pub fn block_state<T: Any>(ctx: &mut dyn RequestCtx, owner: usize) -> Option<&mut T> {
    ctx.block_states()
        .iter_mut()
        .find(|(id, state)| *id == owner && state.is::<T>())
        .and_then(|(_, state)| state.downcast_mut())
}

/// Keeps `state` for the block `owner` for the current request, replacing what it kept
pub fn set_block_state<T: Any + Send + Sync>(ctx: &mut dyn RequestCtx, owner: usize, state: T) {
    let states = ctx.block_states();
    states.retain(|(id, kept)| !(*id == owner && kept.is::<T>()));
    states.push((owner, Box::new(state)));
}
