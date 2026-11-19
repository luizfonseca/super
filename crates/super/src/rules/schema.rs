//! What rules accept (ADR 0007), for the admin UI's rule builder: the fields a condition tests,
//! the operators each takes and the values they need, and the actions of `then`. A test compiles
//! every field with every operator, so this can't say more or less than the compiler accepts.

use std::borrow::Cow;

use schemars::{json_schema, Schema, SchemaGenerator};
use serde::Serialize;
use serde_json::Value;

use super::fields;

/// A field a condition tests
#[derive(Debug, Serialize)]
pub struct Field {
    pub name: &'static str,
    /// Tested by name (`header "x-api-key"`)
    pub named: bool,
    pub description: &'static str,
    pub operators: &'static [&'static str],
}

/// What an operator compares a field with
#[derive(Debug, Serialize)]
pub struct Operator {
    pub name: &'static str,
    /// `text` (one value), `list` (one or more) or `bool`
    pub value: &'static str,
    pub description: &'static str,
}

pub(super) const EQUALITY: &[&str] = &["eq", "neq", "in", "not_in"];

/// In the order the admin's editor lists them
pub const FIELDS: &[Field] = &[
    fields::method::SCHEMA,
    fields::text::HOST,
    fields::text::PATH,
    fields::text::QUERY,
    fields::text::HEADER,
    fields::text::COOKIE,
    fields::client::SCHEMA,
    fields::time::DATE,
    fields::time::WEEKDAY,
    fields::time::TIME,
];

pub const OPERATORS: &[Operator] = &[
    Operator {
        name: "eq",
        value: "text",
        description: "is",
    },
    Operator {
        name: "neq",
        value: "text",
        description: "is not",
    },
    Operator {
        name: "in",
        value: "list",
        description: "is one of",
    },
    Operator {
        name: "not_in",
        value: "list",
        description: "is none of",
    },
    Operator {
        name: "prefix",
        value: "list",
        description: "starts with one of",
    },
    Operator {
        name: "suffix",
        value: "list",
        description: "ends with one of",
    },
    Operator {
        name: "contains",
        value: "list",
        description: "contains one of",
    },
    Operator {
        name: "wildcard",
        value: "text",
        description: "matches (* is any text)",
    },
    Operator {
        name: "matches",
        value: "text",
        description: "matches the regular expression",
    },
    Operator {
        name: "exists",
        value: "bool",
        description: "is there",
    },
    Operator {
        name: "gte",
        value: "text",
        description: "is at or after",
    },
    Operator {
        name: "gt",
        value: "text",
        description: "is after",
    },
    Operator {
        name: "lt",
        value: "text",
        description: "is before",
    },
    Operator {
        name: "lte",
        value: "text",
        description: "is at or before",
    },
    // A list in the admin's editor; exactly two values in the configuration's schema
    Operator {
        name: "between",
        value: "list",
        description: "is from the first until the second",
    },
];

/// Fields, operators, how conditions combine, and the actions of `then`
pub fn schema() -> Value {
    serde_json::json!({
        "fields": FIELDS,
        "operators": OPERATORS,
        "groups": [
            { "name": "and", "description": "all of" },
            { "name": "or", "description": "any of" },
            { "name": "not", "description": "none of" },
        ],
        "actions": super::actions::schema(),
    })
}

/// One rule, or a list of them (HCL gives one `rule` block as an object, and several as a list),
/// as JSON Schema, for the configuration's schema
pub struct RuleList;

impl schemars::JsonSchema for RuleList {
    fn schema_name() -> Cow<'static, str> {
        "RuleList".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let rule = generator.subschema_for::<RuleSchema>();
        json_schema!({ "anyOf": [rule, { "type": "array", "items": rule }] })
    }
}

/// `rule "<name>" { ... }`: its conditions, in `and`, `or` or `not`, and `then`
struct RuleSchema;

impl schemars::JsonSchema for RuleSchema {
    fn schema_name() -> Cow<'static, str> {
        "Rule".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let conditions = generator.subschema_for::<Conditions>();
        let then = generator.subschema_for::<super::actions::ThenBlock>();
        json_schema!({
            "type": "object",
            "description": "A rule (ADR 0007): what requests it matches, and what is done to them",
            "required": ["name", "then"],
            "additionalProperties": false,
            "properties": {
                "name": { "type": "string", "description": "What the rule is for" },
                "and": conditions,
                "or": conditions,
                "not": conditions,
                "then": then,
            },
        })
    }
}

/// The inside of `and`, `or` and `not`: groups, and tests of a field with one operator
struct Conditions;

impl schemars::JsonSchema for Conditions {
    fn schema_name() -> Cow<'static, str> {
        "RuleConditions".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let group = one_or_many(&generator.subschema_for::<Conditions>().to_value());
        let mut properties = serde_json::Map::new();
        for name in ["and", "or", "not"] {
            properties.insert(name.to_string(), group.clone());
        }
        for field in FIELDS {
            properties.insert(field.name.to_string(), field_schema(field));
        }
        json_schema!({
            "type": "object",
            "minProperties": 1,
            "additionalProperties": false,
            "properties": properties,
        })
    }
}

/// A field's test (`path { prefix = "/api" }`), or for a named field, its tests by name
/// (`header "x-api-key" { exists = true }`)
fn field_schema(field: &Field) -> Value {
    let operators: serde_json::Map<String, Value> = OPERATORS
        .iter()
        .filter(|operator| field.operators.contains(&operator.name))
        .map(|operator| (operator.name.to_string(), operator_schema(operator)))
        .collect();
    let test = serde_json::json!({
        "type": "object",
        "description": field.description,
        "minProperties": 1,
        "maxProperties": 1,
        "additionalProperties": false,
        "properties": operators,
    });
    if !field.named {
        return one_or_many(&test);
    }
    serde_json::json!({
        "type": "object",
        "description": field.description,
        "additionalProperties": one_or_many(&test),
    })
}

/// One block, or several written side by side (HCL gives them as a list)
fn one_or_many(schema: &Value) -> Value {
    serde_json::json!({ "anyOf": [schema, { "type": "array", "items": schema }] })
}

/// What an operator takes: a text, one or more texts, or true/false
fn operator_schema(operator: &Operator) -> Value {
    let description = operator.description;
    if operator.name == "between" {
        return serde_json::json!({
            "description": description,
            "type": "array",
            "items": { "type": "string" },
            "minItems": 2,
            "maxItems": 2,
        });
    }
    match operator.value {
        "list" => serde_json::json!({
            "description": description,
            "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }],
        }),
        "bool" => serde_json::json!({ "description": description, "type": "boolean" }),
        _ => serde_json::json!({ "description": description, "type": "string" }),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Field, Operator, FIELDS, OPERATORS};
    use crate::rules::{Rule, Rules};

    /// A value each kind of field takes, for `operator`
    fn value(field: &str, operator: &str) -> serde_json::Value {
        let one = match field {
            "method" => json!("GET"),
            "client" => json!("10.0.0.1"),
            "date" => json!("2026-11-21"),
            "weekday" => json!("mon"),
            "time" => json!("02:00"),
            _ => json!("x"),
        };
        if operator == "between" {
            return match field {
                "date" => json!(["2026-11-21", "2026-11-28"]),
                "time" => json!(["02:00", "04:00"]),
                _ => json!([one, one]),
            };
        }
        match OPERATORS
            .iter()
            .find(|known| known.name == operator)
            .map(|known| known.value)
        {
            Some("list") => json!([one]),
            Some("bool") => json!(true),
            _ => one,
        }
    }

    /// A condition testing `field` with `operator`
    fn condition(field: &Field, operator: &Operator) -> serde_json::Value {
        let test = json!({ operator.name: value(field.name, operator.name) });
        if field.named {
            json!({ field.name: { "x-name": test } })
        } else {
            json!({ field.name: test })
        }
    }

    #[test]
    fn test_time_fields_and_their_operators() {
        let operators = |name: &str| {
            FIELDS
                .iter()
                .find(|field| field.name == name)
                .map(|field| field.operators.to_vec())
        };
        let windows = Some(vec!["gte", "gt", "lt", "lte", "between"]);
        assert_eq!(operators("date"), windows);
        assert_eq!(operators("time"), windows);
        assert_eq!(
            operators("weekday"),
            Some(vec!["eq", "neq", "in", "not_in"])
        );
        // Exactly [from, until]
        let between = OPERATORS
            .iter()
            .find(|operator| operator.name == "between")
            .map(super::operator_schema)
            .unwrap();
        assert_eq!(between["type"], "array");
        assert_eq!(
            (between["minItems"].clone(), between["maxItems"].clone()),
            (json!(2), json!(2))
        );
    }

    #[test]
    fn test_fields_take_exactly_their_operators() {
        for field in FIELDS {
            for operator in OPERATORS {
                let condition = condition(field, operator);
                let rule =
                    Rule(json!({ "name": "a", "and": condition, "then": { "respond": {} } }));

                let compiled = Rules::compile(&[rule], "");

                let listed = field.operators.contains(&operator.name);
                assert_eq!(
                    compiled.is_ok(),
                    listed,
                    "{} {}: {:?}",
                    field.name,
                    operator.name,
                    compiled.err()
                );
            }
        }
    }

    #[test]
    fn test_actions() {
        let schema = super::schema();
        let actions = schema["actions"].as_object().unwrap();
        for name in [
            "respond",
            "redirect",
            "rate_limit",
            "basic_auth",
            "headers",
            "method",
        ] {
            assert!(actions.contains_key(name), "{name}");
        }
        // Every action is one `then` takes
        for name in actions.keys() {
            let rule = Rule(
                json!({ "name": "a", "and": { "method": { "eq": "GET" } }, "then": { name: {} } }),
            );
            let err = Rules::compile(&[rule], "").err().unwrap_or_default();
            assert!(!err.contains("unknown field"), "{name}: {err}");
        }
    }
}
