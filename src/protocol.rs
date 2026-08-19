use serde::{Deserialize, Serialize};

/// Wire protocol version. Semver: minor = additive, major = breaking.
/// See `docs/protocol-spec.md` for what counts as additive vs breaking.
pub const PROTOCOL_VERSION: &str = "1.0";

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Request {
    Get {
        key: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        force: bool,
        #[serde(default)]
        wait: bool,
    },
    Refresh {
        key: String,
        #[serde(default)]
        path: Option<String>,
    },
    Context {
        path: String,
    },
    Status,
    Put {
        key: String,
        #[serde(default)]
        data: Option<serde_json::Value>,
        #[serde(default)]
        ttl: Option<String>,
        #[serde(default)]
        path: Option<String>,
    },
    Watch {
        key: String,
        #[serde(default)]
        path: Option<String>,
    },
    Introspect {
        subject: IntrospectSubject,
        #[serde(default)]
        duration_secs: Option<u64>,
    },
    /// First op a client should send on a new connection. Returns the
    /// daemon's protocol version and build version so clients can verify
    /// compatibility before sending further ops.
    Hello,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IntrospectSubject {
    Daemon,
    Providers,
    Config,
    Cache,
    Lifecycle,
    Watches,
    Timers,
    Demand,
    Procs,
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

/// Split a key on the FIRST dot only: `"git.branch"` → `("git", Some("branch"))`,
/// `"battery"` → `("battery", None)`. This does NOT understand `provider.source`
/// or `provider.source.field` addressing — use `crate::query::parse_key` for
/// source-aware parsing. Retained for `Refresh`, which only needs the provider.
pub fn split_key(key: &str) -> (&str, Option<&str>) {
    match key.split_once('.') {
        Some((provider, field)) => (provider, Some(field)),
        None => (key, None),
    }
}
