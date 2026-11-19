//! Reading the fields of a request, once each, and running every test of a field on it. Each
//! kind of field has its module: how its values are written, how its tests run, and its entry in
//! the rules' schema.

pub(super) mod client;
pub(super) mod method;
pub(super) mod text;
pub(super) mod time;

use std::{borrow::Cow, net::IpAddr};

use http::{HeaderName, Method};
use serde_json::Value;

pub(super) use text::lowercase;

use super::compile::{Field, Group, Test};

/// What rules read of a request: implemented for the proxy's requests, and for requests built in
/// tests and benchmarks
pub(crate) trait Request {
    fn method(&self) -> &Method;
    /// Without its port
    fn host(&self) -> &str;
    /// Normalized, as locations read it: `//a/./b` and `/%61/b` are `/a/b`
    fn path(&self) -> &str;
    fn query(&self) -> Option<&str>;
    /// All the values of a header, joined with `, `
    fn header(&self, name: &HeaderName) -> Option<Cow<'_, str>>;
    /// The client, behind trusted proxies the one they forwarded
    fn client(&self) -> Option<IpAddr>;
    /// Now, in Unix seconds (UTC)
    fn now(&self) -> u64;
}

/// What `operator` checks with `value`, on `field`
pub(super) fn parse(field: &Field, operator: &str, value: &Value) -> Result<Test, String> {
    match field {
        Field::Method => method::parse(operator, value),
        Field::Client => client::parse(operator, value),
        Field::Date | Field::Weekday | Field::Time => time::parse(field, operator, value),
        Field::Host | Field::Path | Field::Query(_) | Field::Header(_) | Field::Cookie(_) => {
            text::parse(field, operator, value)
        }
    }
}

/// Runs one test of a request-line field (method, host, path, client), reading the field again
pub(super) fn evaluate_one(field: &Field, test: &Test, request: &impl Request) -> bool {
    match field {
        Field::Method => method::holds(test, request.method()),
        Field::Client => client::holds(test, client::read(request)),
        Field::Host => text::holds(test, Some(&lowercase(request.host()))),
        Field::Path => text::holds(test, Some(request.path())),
        // Never lazy: read once for all their tests
        _ => false,
    }
}

/// Runs the tests of `group` on `request`, reading its field once: `set` gets each test's result
pub(super) fn evaluate(group: &Group, request: &impl Request, mut set: impl FnMut(u32, bool)) {
    match &group.field {
        Field::Method => {
            let method = request.method();
            for (index, test) in &group.tests {
                set(*index, method::holds(test, method));
            }
        }
        Field::Client => {
            let client = client::read(request);
            for (index, test) in &group.tests {
                set(*index, client::holds(test, client));
            }
        }
        Field::Host => text::evaluate(group, Some(&lowercase(request.host())), set),
        Field::Path => text::evaluate(group, Some(request.path()), set),
        Field::Query(name) => {
            let value = text::query(request.query(), name);
            text::evaluate(group, value.as_deref(), set);
        }
        Field::Header(name) => text::evaluate(group, request.header(name).as_deref(), set),
        Field::Cookie(name) => {
            let cookies = request.header(&http::header::COOKIE);
            text::evaluate(group, text::cookie(cookies.as_deref(), name), set);
        }
        Field::Date | Field::Weekday | Field::Time => {
            let value = time::read(&group.field, request.now());
            for (index, test) in &group.tests {
                set(*index, time::holds(test, value));
            }
        }
    }
}

fn unsupported(field: &str, supported: &str) -> String {
    format!("isn't an operator of {field} (it takes {supported})")
}

fn list(value: &Value) -> Result<&Vec<Value>, String> {
    value
        .as_array()
        .filter(|values| !values.is_empty())
        .ok_or_else(|| "must be a list of values".to_string())
}
