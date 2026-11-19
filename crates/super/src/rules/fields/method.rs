//! `method`: the request's method, ignoring case

use http::Method;
use serde_json::Value;

use super::{list, unsupported};
use crate::rules::{compile::Test, schema};

pub(in crate::rules) const SCHEMA: schema::Field = schema::Field {
    name: "method",
    named: false,
    description: "The method (GET, POST...), ignoring case",
    operators: schema::EQUALITY,
};

/// A test of `operator` with `value`
pub(super) fn parse(operator: &str, value: &Value) -> Result<Test, String> {
    let methods = match operator {
        "eq" | "neq" => vec![method(value)?],
        "in" | "not_in" => list(value)?.iter().map(method).collect::<Result<_, _>>()?,
        _ => return Err(unsupported("method", "`eq`, `neq`, `in` and `not_in`")),
    };
    let negate = matches!(operator, "neq" | "not_in");
    Ok(Test::Method { methods, negate })
}

/// Whether `test` holds for `method`
pub(super) fn holds(test: &Test, method: &Method) -> bool {
    match test {
        Test::Method { methods, negate } => methods.contains(method) != *negate,
        _ => false,
    }
}

fn method(value: &Value) -> Result<Method, String> {
    let name = value.as_str().ok_or("must be a method, such as \"GET\"")?;
    Method::from_bytes(name.to_ascii_uppercase().as_bytes())
        .map_err(|_| format!("{name:?} isn't a method"))
}
