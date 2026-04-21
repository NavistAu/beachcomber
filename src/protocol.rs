use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Json,
    Text,
    Sh,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Request {
    Get {
        key: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        format: Format,
    },
    Refresh {
        key: String,
        #[serde(default)]
        path: Option<String>,
    },
    Context {
        path: String,
    },
    List,
    Status,
    Store {
        key: String,
        data: serde_json::Value,
        #[serde(default)]
        ttl: Option<String>,
        #[serde(default)]
        path: Option<String>,
    },
    Watch {
        key: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        format: Format,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(data: serde_json::Value, age_ms: u128, stale: bool) -> Self {
        Self {
            ok: true,
            data: Some(data),
            age_ms: Some(age_ms),
            stale: Some(stale),
            error: None,
        }
    }

    pub fn miss() -> Self {
        Self {
            ok: true,
            data: None,
            age_ms: None,
            stale: None,
            error: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            age_ms: None,
            stale: None,
            error: Some(msg.into()),
        }
    }
}

pub fn split_key(key: &str) -> (&str, Option<&str>) {
    match key.split_once('.') {
        Some((provider, field)) => (provider, Some(field)),
        None => (key, None),
    }
}
