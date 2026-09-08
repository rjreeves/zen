use serde_json::{Map as JsonMap, Value as JsonValue};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Secret(String),
    /// A value that must never be stored as plaintext (e.g. a `Secret` about
    /// to be journaled) - already-encrypted ciphertext bytes (see
    /// `crate::dpapi`) carried through the value/JSON machinery so a durable
    /// journal can persist it and recover it byte-for-byte on replay without
    /// ever writing the plaintext to disk.
    Encrypted(Vec<u8>),
    /// General-purpose binary data - `FsRequest::ReadBytes`'s own success
    /// value, and `WriteBytes`'s input. Distinct from `Encrypted` above:
    /// that variant is reserved for ciphertext specifically (see its own
    /// doc comment); this one carries ordinary bytes (e.g. a binary file's
    /// real contents) with no encryption meaning attached at all.
    Bytes(Vec<u8>),
    Object(HashMap<String, Value>),
    List(Vec<Value>),
}

impl Value {
    #[allow(dead_code)]
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&str> {
        match self {
            Value::String(s) | Value::Secret(s) => Some(s),
            _ => None,
        }
    }

    pub fn redacted(&self) -> &'static str {
        "[secret]"
    }
}

/// The single object key used to carry `Value::Encrypted` ciphertext through
/// JSON (as a plain array of 0-255 byte values, not base64, to keep this a
/// zero-new-dependency change). Kept `__zen_`-prefixed so it can't collide
/// with an ordinary workflow-authored object key by accident.
const ENCRYPTED_JSON_KEY: &str = "__zen_encrypted";

/// `Value::Bytes`'s own JSON carrier key, same array-of-byte-values shape
/// and zero-new-dependency reasoning as `ENCRYPTED_JSON_KEY` above - a
/// distinct key (not reusing `__zen_encrypted`) so the two variants stay
/// distinguishable on the way back through `json_to_value`.
const BYTES_JSON_KEY: &str = "__zen_bytes";

pub fn value_to_json(value: &Value) -> JsonValue {
    match value {
        Value::Null => JsonValue::Null,
        Value::Bool(b) => JsonValue::Bool(*b),
        Value::Number(n) => serde_json::json!(n),
        Value::String(s) => JsonValue::String(s.clone()),
        Value::Secret(_) => JsonValue::String("[secret]".into()),
        Value::Encrypted(bytes) => bytes_to_json_object(ENCRYPTED_JSON_KEY, bytes),
        Value::Bytes(bytes) => bytes_to_json_object(BYTES_JSON_KEY, bytes),
        Value::List(items) => JsonValue::Array(items.iter().map(value_to_json).collect()),
        Value::Object(map) => {
            let mut obj = JsonMap::new();
            for (key, value) in map {
                obj.insert(key.clone(), value_to_json(value));
            }
            JsonValue::Object(obj)
        }
    }
}

pub fn json_to_value(json: JsonValue) -> Value {
    match json {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(b) => Value::Bool(b),
        JsonValue::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
        JsonValue::String(s) => Value::String(s),
        JsonValue::Array(items) => Value::List(items.into_iter().map(json_to_value).collect()),
        JsonValue::Object(map) => match bytes_from_json_object(&map, ENCRYPTED_JSON_KEY) {
            Some(bytes) => Value::Encrypted(bytes),
            None => match bytes_from_json_object(&map, BYTES_JSON_KEY) {
                Some(bytes) => Value::Bytes(bytes),
                None => {
                    let mut object = HashMap::new();
                    for (key, value) in map {
                        object.insert(key, json_to_value(value));
                    }
                    Value::Object(object)
                }
            },
        },
    }
}

/// `Value::Encrypted`/`Value::Bytes`'s shared JSON encoding: a single-key
/// object whose value is a plain JSON array of `0..=255` integers - no
/// base64, matching `ENCRYPTED_JSON_KEY`'s own zero-new-dependency
/// reasoning (now shared by both, not just `Encrypted`, since it's the
/// same real shape either way).
fn bytes_to_json_object(key: &str, bytes: &[u8]) -> JsonValue {
    let mut obj = JsonMap::new();
    let array = bytes.iter().map(|b| JsonValue::Number((*b).into())).collect();
    obj.insert(key.into(), JsonValue::Array(array));
    JsonValue::Object(obj)
}

/// Recognizes exactly the `{"<key>": [<byte>, ...]}` shape `bytes_to_json_object`
/// produces - a single key named `key` whose value is a JSON array where
/// every element is an integer in `0..=255`. Anything else (extra keys, a
/// differently-shaped value, out-of-range or non-integer numbers) returns
/// `None` so the caller falls through to plain `Object` handling (or the
/// other byte-carrying key) instead of misinterpreting or corrupting an
/// ordinary object that merely happens to reuse one of these keys.
fn bytes_from_json_object(map: &JsonMap<String, JsonValue>, key: &str) -> Option<Vec<u8>> {
    if map.len() != 1 {
        return None;
    }
    let array = map.get(key)?.as_array()?;
    array
        .iter()
        .map(|item| item.as_u64().filter(|n| *n <= 255).map(|n| n as u8))
        .collect()
}

pub fn value_to_echo_string(value: Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s,
        Value::Secret(_) => "[secret]".into(),
        Value::Encrypted(_) => "[encrypted]".into(),
        Value::Bytes(bytes) => format!("[{} bytes]", bytes.len()),
        other => format!("{:?}", other),
    }
}

pub fn eq_vals(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

/// Recognizes the `{ secret: "name" }` symbolic secret reference shape used
/// in workflow step env and exec env config, returning the referenced name.
/// Resolution (looking the name up in a secret store) is left to the caller.
pub fn secret_reference_name(value: &Value) -> Option<&str> {
    match value {
        Value::Object(entry) if entry.len() == 1 => match entry.get("secret") {
            Some(Value::String(name)) if !name.is_empty() => Some(name.as_str()),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_value_round_trips_through_json() {
        let bytes: Vec<u8> = vec![0, 1, 2, 254, 255, 42];
        let value = Value::Encrypted(bytes.clone());

        let json = value_to_json(&value);
        assert_eq!(json, serde_json::json!({ "__zen_encrypted": [0, 1, 2, 254, 255, 42] }));

        let round_tripped = json_to_value(json);
        match round_tripped {
            Value::Encrypted(decoded) => assert_eq!(decoded, bytes),
            other => panic!("expected Value::Encrypted, got {:?}", other),
        }
    }

    #[test]
    fn encrypted_value_round_trips_when_empty() {
        let value = Value::Encrypted(Vec::new());
        let json = value_to_json(&value);
        let round_tripped = json_to_value(json);
        match round_tripped {
            Value::Encrypted(decoded) => assert!(decoded.is_empty()),
            other => panic!("expected Value::Encrypted, got {:?}", other),
        }
    }

    #[test]
    fn object_with_similarly_named_key_but_non_array_value_is_not_misinterpreted() {
        let json = serde_json::json!({ "__zen_encrypted": "not an array" });
        match json_to_value(json) {
            Value::Object(map) => {
                assert_eq!(map.len(), 1);
                assert!(matches!(map.get("__zen_encrypted"), Some(Value::String(s)) if s == "not an array"));
            }
            other => panic!("expected plain Value::Object, got {:?}", other),
        }
    }

    #[test]
    fn object_with_similarly_named_key_but_out_of_range_bytes_is_not_misinterpreted() {
        let json = serde_json::json!({ "__zen_encrypted": [1, 2, 999] });
        match json_to_value(json) {
            Value::Object(map) => {
                assert_eq!(map.len(), 1);
                assert!(map.contains_key("__zen_encrypted"));
            }
            other => panic!("expected plain Value::Object, got {:?}", other),
        }
    }

    #[test]
    fn object_with_encrypted_key_plus_extra_keys_is_not_misinterpreted() {
        let json = serde_json::json!({ "__zen_encrypted": [1, 2, 3], "other": "field" });
        match json_to_value(json) {
            Value::Object(map) => {
                assert_eq!(map.len(), 2);
            }
            other => panic!("expected plain Value::Object, got {:?}", other),
        }
    }

    #[test]
    fn encrypted_value_displays_as_redacted_in_echo_string() {
        assert_eq!(value_to_echo_string(Value::Encrypted(vec![1, 2, 3])), "[encrypted]");
    }

    #[test]
    fn bytes_value_round_trips_through_json() {
        let bytes: Vec<u8> = vec![0, 1, 2, 254, 255, 42];
        let value = Value::Bytes(bytes.clone());

        let json = value_to_json(&value);
        assert_eq!(json, serde_json::json!({ "__zen_bytes": [0, 1, 2, 254, 255, 42] }));

        let round_tripped = json_to_value(json);
        match round_tripped {
            Value::Bytes(decoded) => assert_eq!(decoded, bytes),
            other => panic!("expected Value::Bytes, got {:?}", other),
        }
    }

    #[test]
    fn bytes_value_round_trips_when_empty() {
        let value = Value::Bytes(Vec::new());
        let json = value_to_json(&value);
        let round_tripped = json_to_value(json);
        match round_tripped {
            Value::Bytes(decoded) => assert!(decoded.is_empty()),
            other => panic!("expected Value::Bytes, got {:?}", other),
        }
    }

    #[test]
    fn bytes_and_encrypted_use_distinct_json_keys_and_never_cross_decode() {
        let encrypted_json = value_to_json(&Value::Encrypted(vec![9, 9, 9]));
        let bytes_json = value_to_json(&Value::Bytes(vec![9, 9, 9]));
        assert_ne!(encrypted_json, bytes_json);
        assert!(matches!(json_to_value(encrypted_json), Value::Encrypted(_)));
        assert!(matches!(json_to_value(bytes_json), Value::Bytes(_)));
    }

    #[test]
    fn bytes_value_displays_with_a_length_summary_in_echo_string() {
        assert_eq!(value_to_echo_string(Value::Bytes(vec![1, 2, 3])), "[3 bytes]");
    }
}
