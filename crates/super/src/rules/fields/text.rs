//! Text fields: `host`, `path`, `query "<name>"`, `header "<name>"` and `cookie "<name>"`

use std::borrow::Cow;

use serde_json::Value;

use super::{list, unsupported};
use crate::rules::{
    compile::{Field, Group, Test},
    schema,
    text_set::{Ends, Exact, Parts},
};

const TEXT: &[&str] = &[
    "eq", "neq", "in", "not_in", "prefix", "suffix", "contains", "wildcard", "matches",
];
const NAMED_TEXT: &[&str] = &[
    "eq", "neq", "in", "not_in", "prefix", "suffix", "contains", "wildcard", "matches", "exists",
];

pub(in crate::rules) const HOST: schema::Field = schema::Field {
    name: "host",
    named: false,
    description: "The host the request is for, without its port, ignoring case",
    operators: TEXT,
};
pub(in crate::rules) const PATH: schema::Field = schema::Field {
    name: "path",
    named: false,
    description: "The path, as used to choose locations",
    operators: TEXT,
};
pub(in crate::rules) const QUERY: schema::Field = schema::Field {
    name: "query",
    named: true,
    description: "The value of a query parameter, decoded",
    operators: NAMED_TEXT,
};
pub(in crate::rules) const HEADER: schema::Field = schema::Field {
    name: "header",
    named: true,
    description: "The value of a request header; names ignore case",
    operators: NAMED_TEXT,
};
pub(in crate::rules) const COOKIE: schema::Field = schema::Field {
    name: "cookie",
    named: true,
    description: "The value of a cookie",
    operators: NAMED_TEXT,
};

#[derive(Debug)]
pub(in crate::rules) enum TextTest {
    Eq(String),
    In(Exact),
    /// Any of them
    Prefix(Ends),
    Suffix(Ends),
    Contains(Parts),
    /// The parts between `*`s
    Wildcard(Vec<String>),
    /// Tested with the regex set of its field
    Matches,
    Exists(bool),
}

/// A test of `operator` with `value`, on the text field `field`
pub(super) fn parse(field: &Field, operator: &str, value: &Value) -> Result<Test, String> {
    // Hosts are compared ignoring case
    let fold = |text: String| {
        if *field == Field::Host {
            text.to_ascii_lowercase()
        } else {
            text
        }
    };
    let test = match operator {
        "eq" | "neq" => TextTest::Eq(fold(text(value)?)),
        "in" | "not_in" => TextTest::In(Exact::new(
            list(value)?
                .iter()
                .map(|value| text(value).map(fold))
                .collect::<Result<_, _>>()?,
        )),
        "prefix" => TextTest::Prefix(Ends::new(texts(value, fold)?)),
        "suffix" => TextTest::Suffix(Ends::new(texts(value, fold)?)),
        "contains" => TextTest::Contains(Parts::new(texts(value, fold)?)?),
        "wildcard" => TextTest::Wildcard(
            fold(text(value)?)
                .split('*')
                .map(ToString::to_string)
                .collect(),
        ),
        "matches" => {
            value.as_str().ok_or("must be a regular expression")?;
            TextTest::Matches
        }
        "exists" => exists(field, value)?,
        "gt" | "gte" | "lt" | "lte" | "between" => {
            return Err("compares dates and times, which this field isn't".to_string())
        }
        other => return Err(format!("`{other}` isn't an operator")),
    };
    let negate = matches!(operator, "neq" | "not_in");
    Ok(Test::Text { test, negate })
}

/// `exists`: for the fields a request can lack
fn exists(field: &Field, value: &Value) -> Result<TextTest, String> {
    if matches!(field, Field::Host | Field::Path) {
        return Err(unsupported(
            "host and path",
            "all but `exists`: they're always there",
        ));
    }
    Ok(TextTest::Exists(
        value.as_bool().ok_or("must be true or false")?,
    ))
}

/// A text, or a list of them, each changed with `fold`
fn texts(value: &Value, fold: impl Fn(String) -> String) -> Result<Vec<String>, String> {
    match value {
        Value::Array(_) => list(value)?
            .iter()
            .map(|value| text(value).map(&fold))
            .collect(),
        value => Ok(vec![fold(text(value)?)]),
    }
}

fn text(value: &Value) -> Result<String, String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        Value::Bool(boolean) => Ok(boolean.to_string()),
        _ => Err("must be text".to_string()),
    }
}

pub(in crate::rules) fn lowercase(host: &str) -> Cow<'_, str> {
    if host.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(host.to_ascii_lowercase())
    } else {
        Cow::Borrowed(host)
    }
}

/// Whether `test` holds for `value` (`None`: the field isn't there)
pub(super) fn holds(test: &Test, value: Option<&str>) -> bool {
    match test {
        Test::Text { test, negate } => text_test(test, value) != *negate,
        _ => false,
    }
}

/// A text test on a value (`None`: the field isn't there), before `negate`
fn text_test(test: &TextTest, value: Option<&str>) -> bool {
    match (test, value) {
        (TextTest::Exists(exists), value) => value.is_some() == *exists,
        // `matches` runs with the group's set
        (TextTest::Matches, _) | (_, None) => false,
        (TextTest::Eq(expected), Some(value)) => value == expected,
        (TextTest::In(expected), Some(value)) => expected.holds(value),
        (TextTest::Prefix(prefixes), Some(value)) => prefixes.starts(value),
        (TextTest::Suffix(suffixes), Some(value)) => suffixes.ends(value),
        (TextTest::Contains(parts), Some(value)) => parts.found_in(value),
        (TextTest::Wildcard(parts), Some(value)) => wildcard(parts, value),
    }
}

/// Runs the tests of `group` on `value`, its patterns as one set: `set` gets each result
pub(super) fn evaluate(group: &Group, value: Option<&str>, mut set: impl FnMut(u32, bool)) {
    for (index, test) in &group.tests {
        let Test::Text { test, negate } = test else {
            continue;
        };
        // Run with the group's set, below
        if matches!(test, TextTest::Matches) {
            continue;
        }
        set(*index, text_test(test, value) != *negate);
    }
    if let Some((patterns, tests)) = &group.regexes {
        // Most values match none of the patterns: finding that out stops at the first match,
        // and only then are the patterns that matched worked out
        let matched = value
            .filter(|value| patterns.is_match(value))
            .map(|value| patterns.matches(value));
        for (pattern, index) in tests.iter().enumerate() {
            set(
                *index,
                matched
                    .as_ref()
                    .is_some_and(|matched| matched.matched(pattern)),
            );
        }
    }
}

/// `parts` (the text between `*`s) in order: the first at the start, the last at the end
fn wildcard(parts: &[String], value: &str) -> bool {
    let (Some(first), Some(last)) = (parts.first(), parts.last()) else {
        return false;
    };
    if parts.len() == 1 {
        return value == first;
    }
    let Some(mut rest) = value.strip_prefix(first.as_str()) else {
        return false;
    };
    let Some(between) = rest.strip_suffix(last.as_str()) else {
        return false;
    };
    rest = between;
    for part in parts.get(1..parts.len() - 1).unwrap_or_default() {
        match rest
            .find(part.as_str())
            .and_then(|at| rest.get(at + part.len()..))
        {
            Some(after) => rest = after,
            None => return false,
        }
    }
    true
}

/// The value of the first query parameter `name`, decoded (`%20`, `+`)
pub(super) fn query<'a>(query: Option<&'a str>, name: &str) -> Option<Cow<'a, str>> {
    query?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (decode(key) == name).then(|| decode(value))
    })
}

fn decode(text: &str) -> Cow<'_, str> {
    if !text.contains(['%', '+']) {
        return Cow::Borrowed(text);
    }
    let mut decoded = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        rest = tail;
        match byte {
            b'+' => decoded.push(b' '),
            b'%' => decode_percent(&mut rest, &mut decoded),
            byte => decoded.push(byte),
        }
    }
    Cow::Owned(String::from_utf8_lossy(&decoded).into_owned())
}

/// After a `%`: the byte of the two hex digits in `rest` (taken), or the `%` itself
fn decode_percent(rest: &mut &[u8], decoded: &mut Vec<u8>) {
    let hex = rest
        .get(..2)
        .and_then(|hex| std::str::from_utf8(hex).ok())
        .and_then(|hex| u8::from_str_radix(hex, 16).ok());
    match hex {
        Some(byte) => {
            decoded.push(byte);
            *rest = rest.get(2..).unwrap_or_default();
        }
        None => decoded.push(b'%'),
    }
}

/// The value of cookie `name` in a `Cookie` header
pub(super) fn cookie<'a>(cookies: Option<&'a str>, name: &str) -> Option<&'a str> {
    cookies?.split(';').find_map(|pair| {
        let (key, value) = pair.trim().split_once('=')?;
        (key == name).then_some(value)
    })
}

#[cfg(test)]
mod tests {
    use super::{cookie, decode, query, wildcard};

    fn parts(pattern: &str) -> Vec<String> {
        pattern.split('*').map(ToString::to_string).collect()
    }

    #[test]
    fn test_wildcard() {
        assert!(wildcard(&parts("/api/*"), "/api/users"));
        assert!(wildcard(&parts("/api/*"), "/api/"));
        assert!(!wildcard(&parts("/api/*"), "/apis"));
        assert!(wildcard(&parts("*.example.com"), "a.b.example.com"));
        assert!(!wildcard(&parts("*.example.com"), "example.com"));
        assert!(wildcard(&parts("/a/*/c/*"), "/a/b/c/d"));
        assert!(!wildcard(&parts("/a/*/c"), "/a/b/d"));
        assert!(wildcard(&parts("*"), "anything"));
        assert!(wildcard(&parts("/exact"), "/exact"));
        assert!(!wildcard(&parts("ab*ba"), "aba"));
    }

    #[test]
    fn test_query_and_cookies() {
        assert_eq!(
            query(Some("a=1&q=a+b%21&q=2"), "q").as_deref(),
            Some("a b!")
        );
        assert_eq!(query(Some("flag&b=2"), "flag").as_deref(), Some(""));
        assert_eq!(query(Some("a=1"), "b"), None);
        assert_eq!(query(None, "b"), None);
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");

        assert_eq!(cookie(Some("a=1; beta=1; c=3"), "beta"), Some("1"));
        assert_eq!(cookie(Some("betas=1"), "beta"), None);
    }
}
