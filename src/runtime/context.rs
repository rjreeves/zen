#![allow(dead_code)]

use crate::runtime::values::Value;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Context {
    pub vars: HashMap<String, Value>,
}
