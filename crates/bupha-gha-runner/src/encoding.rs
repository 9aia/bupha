//! The two JSON wrappings the Actions service puts around job data.
//!
//! Everything the broker hands a runner arrives in one of two shapes:
//!
//! * **data** — contexts (`contextData`, `environmentVariables`):
//!   dictionaries as `{"t": 2, "d": [{"k": .., "v": ..}]}`, arrays as
//!   `{"t": 1, "a": [..]}`, everything else as plain JSON.
//! * **token** — workflow-compiled fields (`steps[].inputs`,
//!   `steps[].env`, `displayNameToken`): literals as
//!   `{"type": 0, "lit": ..}` / `{"type": 5, "bool": ..}` /
//!   `{"type": 6, "num": ..}`, sequences `{"type": 1, "seq": [..]}`,
//!   maps `{"type": 2, "map": [{"Key": .., "Value": ..}]}`, and
//!   expressions `{"type": 3, "expr": ..}` which decode back to the
//!   `${{ .. }}` source form. `type` values >= 4 that are not 5/6 are
//!   treated as literals.
//!
//! [`read_token`] / [`read_data`] unwrap one `serde_json::Value` into
//! the plain JSON the service meant; the wire types in [`crate::types`]
//! then deserialize from that.

use serde_json::{Map, Value};

/// Unwrap a data-encoded value (contexts, env maps) to plain JSON.
pub fn read_data(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(read_data).collect()),
        Value::Object(fields) => match fields.get("t").and_then(Value::as_u64) {
            Some(1) => items(fields, "a", read_data),
            // 2 and 5 differ only in key case-folding server side;
            // both arrive as {"k","v"} pairs here.
            Some(_) => entries(fields, "d", "k", "v", read_data),
            None => Value::Object(
                fields
                    .into_iter()
                    .map(|(at, it)| (at, read_data(it)))
                    .collect(),
            ),
        },
        other => other,
    }
}

/// Unwrap a token-encoded value (compiled step fields) to plain JSON.
/// Expressions come back as their `${{ .. }}` source text.
pub fn read_token(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(read_token).collect()),
        Value::Object(mut fields) => match fields.get("type").and_then(Value::as_u64) {
            Some(1) => items(fields, "seq", read_token),
            Some(2) => entries(fields, "map", "Key", "Value", read_token),
            Some(3) => expression(&fields),
            Some(_) => literal(&mut fields),
            None => Value::Object(
                fields
                    .into_iter()
                    .map(|(at, it)| (at, read_token(it)))
                    .collect(),
            ),
        },
        other => other,
    }
}

fn items(mut fields: Map<String, Value>, at: &str, read: fn(Value) -> Value) -> Value {
    match fields.remove(at) {
        Some(Value::Array(items)) => Value::Array(items.into_iter().map(read).collect()),
        _ => Value::Array(Vec::new()),
    }
}

fn entries(
    mut fields: Map<String, Value>,
    under: &str,
    key: &str,
    value: &str,
    read: fn(Value) -> Value,
) -> Value {
    let Some(Value::Array(entries)) = fields.remove(under) else {
        return Value::Object(Map::new());
    };
    Value::Object(
        entries
            .into_iter()
            .filter_map(|entry| {
                let Value::Object(mut entry) = entry else {
                    return None;
                };
                let name = match read(entry.remove(key)?) {
                    Value::String(name) => name,
                    named => named.to_string(),
                };
                Some((name, read(entry.remove(value).unwrap_or(Value::Null))))
            })
            .collect(),
    )
}

/// A literal keeps its payload under a key named after its kind.
fn literal(fields: &mut Map<String, Value>) -> Value {
    for kind in ["lit", "bool", "num"] {
        if let Some(value) = fields.remove(kind) {
            return value;
        }
    }
    Value::Object(std::mem::take(fields))
}

fn expression(fields: &Map<String, Value>) -> Value {
    fields
        .get("expr")
        .and_then(Value::as_str)
        .map(|source| Value::String(format!("${{{{ {source} }}}}")))
        .unwrap_or(Value::Null)
}

/// Decode a whole field: `null`/missing becomes the type's default.
pub fn decode<T: serde::de::DeserializeOwned + Default>(
    value: Option<&Value>,
    read: fn(Value) -> Value,
) -> Result<T, String> {
    match value {
        None | Some(Value::Null) => Ok(T::default()),
        Some(v) => {
            let plain = read(v.clone());
            serde_json::from_value(plain).map_err(|e| format!("decode: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde::Deserialize;
    use std::collections::BTreeMap;

    #[derive(Debug, Default, PartialEq, Deserialize)]
    #[serde(default)]
    struct Step {
        #[serde(rename = "displayNameToken", deserialize_with = "tok_str")]
        name: String,
        #[serde(rename = "inputs", deserialize_with = "tok_map")]
        inputs: BTreeMap<String, String>,
    }

    fn tok_str<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
        let v = Value::deserialize(d)?;
        match read_token(v) {
            Value::String(s) => Ok(s),
            other => Err(serde::de::Error::custom(format!("not a string: {other}"))),
        }
    }

    fn tok_map<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<String, String>, D::Error> {
        let v = Value::deserialize(d)?;
        match read_token(v) {
            Value::Object(m) => m
                .into_iter()
                .map(|(k, v)| {
                    v.as_str()
                        .map(|s| (k, s.to_string()))
                        .ok_or_else(|| serde::de::Error::custom("non-string value"))
                })
                .collect(),
            _ => Ok(BTreeMap::new()),
        }
    }

    #[test]
    fn token_literals_maps_and_expressions_decode() {
        let step: Step = serde_json::from_str(
            r#"{
                "displayNameToken": {"type": 0, "file": 1, "line": 85, "lit": "A step"},
                "inputs": {"type": 2, "map": [
                    {"Key": {"type": 0, "lit": "script"},
                     "Value": {"type": 0, "lit": "exit 3"}},
                    {"Key": {"type": 0, "lit": "expr"},
                     "Value": {"type": 3, "expr": "github.sha"}}
                ]}
            }"#,
        )
        .unwrap();
        assert_eq!(step.name, "A step");
        assert_eq!(step.inputs["script"], "exit 3");
        assert_eq!(step.inputs["expr"], "${{ github.sha }}");
    }

    #[test]
    fn data_dicts_and_arrays_decode() {
        let v = read_data(serde_json::json!({
            "t": 2,
            "d": [
                {"k": "repository", "v": "o/r"},
                {"k": "event", "v": {"t": 1, "a": [1, {"t": 2, "d": [{"k": "a", "v": true}]}]}}
            ]
        }));
        assert_eq!(v["repository"], "o/r");
        assert_eq!(v["event"][0], 1);
        assert_eq!(v["event"][1]["a"], true);
    }
}
