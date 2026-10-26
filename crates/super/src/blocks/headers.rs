//! `headers { response { add = { ... }, remove = [...] } }`: changes response headers

use std::{
    future::{ready, Future},
    str::FromStr,
};

use anyhow::Result;
use http::{HeaderName, HeaderValue};
use super_core::{http::ResponseHeader, proxy::Session};

use crate::config::RouteHeader;

use super_plugins::RequestCtx;

use super::Middleware;

pub(super) struct Headers {
    /// Parsed once: setting them clones a reference, without allocating
    add: Vec<(HeaderName, HeaderValue)>,
    remove: Vec<String>,
}

impl Headers {
    /// `None` when no header is changed
    pub(super) fn new(headers: &RouteHeader) -> Result<Option<Self>, String> {
        let add = headers
            .add
            .iter()
            .flatten()
            .map(|header| {
                let name = HeaderName::from_str(&header.name);
                let value = HeaderValue::from_str(&header.value);
                match (name, value) {
                    (Ok(name), Ok(value)) => Ok((name, value)),
                    _ => Err(format!("headers: {:?} is not a valid header", header.name)),
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let remove = headers
            .remove
            .iter()
            .flatten()
            .map(|header| header.name.to_string())
            .collect::<Vec<_>>();

        if add.is_empty() && remove.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self { add, remove }))
    }
}

impl Middleware for Headers {
    fn response(
        &self,
        _session: &mut Session,
        response: &mut ResponseHeader,
        _ctx: &mut dyn RequestCtx,
    ) -> impl Future<Output = Result<()>> + Send {
        ready(self.change(response))
    }
}

impl Headers {
    /// Adds and removes the location's response headers
    fn change(&self, response: &mut ResponseHeader) -> Result<()> {
        for (name, value) in &self.add {
            response.insert_header(name, value.clone())?;
        }
        for name in &self.remove {
            response.remove_header(name);
        }
        Ok(())
    }
}
