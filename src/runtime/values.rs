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
