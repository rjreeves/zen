use serde_json::{Map as JsonMap, Value as JsonValue};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Secret(String),
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

pub fn value_to_json(value: &Value) -> JsonValue {
    match value {
        Value::Null => JsonValue::Null,
        Value::Bool(b) => JsonValue::Bool(*b),
        Value::Number(n) => serde_json::json!(n),
        Value::String(s) => JsonValue::String(s.clone()),
        Value::Secret(_) => JsonValue::String("[secret]".into()),
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
        JsonValue::Object(map) => {
            let mut object = HashMap::new();
            for (key, value) in map {
                object.insert(key, json_to_value(value));
            }
            Value::Object(object)
        }
    }
}

pub fn value_to_echo_string(value: Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s,
        Value::Secret(_) => "[secret]".into(),
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
