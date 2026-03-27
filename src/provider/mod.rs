pub mod registry;
pub mod hostname;
pub mod user;
pub mod git;
pub mod battery;
pub mod load;
pub mod uptime;
pub mod network;

use serde::{Serialize, Deserialize};
use std::collections::HashMap;

/// Note: `#[serde(untagged)]` means Float values that are whole numbers (e.g., 42.0)
/// may deserialize as Int on a round-trip. This is acceptable for shell state values
/// which are predominantly strings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Value {
    String(String),
    Int(i64),
    Bool(bool),
    Float(f64),
}

impl Value {
    pub fn as_text(&self) -> String {
        match self {
            Value::String(s) => s.clone(),
            Value::Int(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Float(f) => f.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderResult {
    pub fields: HashMap<String, Value>,
}

impl ProviderResult {
    pub fn new() -> Self {
        Self { fields: HashMap::new() }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.get(key)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.fields).unwrap_or(serde_json::Value::Null)
    }

    pub fn to_kv_text(&self) -> String {
        let mut lines: Vec<String> = self.fields.iter()
            .map(|(k, v)| format!("{}={}", k, v.as_text()))
            .collect();
        lines.sort();
        let mut out = lines.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }

    pub fn insert(&mut self, key: impl Into<String>, value: Value) {
        self.fields.insert(key.into(), value);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldSchema {
    pub name: String,
    pub field_type: FieldType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldType {
    String,
    Int,
    Bool,
    Float,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InvalidationStrategy {
    Watch {
        patterns: Vec<String>,
        fallback_poll_secs: Option<u64>,
    },
    Poll {
        interval_secs: u64,
        floor_secs: u64,
    },
    WatchAndPoll {
        patterns: Vec<String>,
        interval_secs: u64,
        floor_secs: u64,
    },
    Once,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderMetadata {
    pub name: String,
    pub fields: Vec<FieldSchema>,
    pub invalidation: InvalidationStrategy,
    pub global: bool,
}

pub trait Provider: Send + Sync {
    fn metadata(&self) -> ProviderMetadata;
    fn execute(&self, path: Option<&str>) -> Option<ProviderResult>;
}
