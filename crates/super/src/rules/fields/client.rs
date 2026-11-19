//! `client`: the client's address, behind trusted proxies the one they forwarded

use std::net::IpAddr;

use ipnet::IpNet;
use serde_json::Value;

use super::{list, unsupported, Request};
use crate::rules::{compile::Test, schema};

pub(in crate::rules) const SCHEMA: schema::Field = schema::Field {
    name: "client",
    named: false,
    description: "The client's address (behind trusted proxies, the one they forwarded)",
    operators: schema::EQUALITY,
};

/// A test of `operator` with `value`
pub(super) fn parse(operator: &str, value: &Value) -> Result<Test, String> {
    let networks = match operator {
        "eq" | "neq" => vec![network(value)?],
        "in" | "not_in" => list(value)?.iter().map(network).collect::<Result<_, _>>()?,
        _ => return Err(unsupported("client", "`eq`, `neq`, `in` and `not_in`")),
    };
    let networks = crate::ip_set::IpSet::new(networks);
    let negate = matches!(operator, "neq" | "not_in");
    Ok(Test::Client { networks, negate })
}

/// The client of `request` as networks hold it: IPv4 as `::ffff:a.b.c.d`, so an IPv4 client
/// over IPv6 is the same
pub(super) fn read(request: &impl Request) -> Option<u128> {
    request.client().map(crate::ip_set::address)
}

/// Whether `test` holds for `client` (as [`read`] gives it)
pub(super) fn holds(test: &Test, client: Option<u128>) -> bool {
    match test {
        Test::Client { networks, negate } => {
            let listed = client.is_some_and(|client| networks.contains_address(client));
            listed != *negate
        }
        _ => false,
    }
}

fn network(value: &Value) -> Result<IpNet, String> {
    let text = value.as_str().ok_or("must be an address or a network")?;
    text.parse::<IpNet>()
        .or_else(|_| text.parse::<IpAddr>().map(IpNet::from))
        .map_err(|_| format!("{text:?} isn't an address or a network (\"10.0.0.0/8\")"))
}
