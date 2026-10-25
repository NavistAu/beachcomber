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
    Status,
    Put {
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
    /// The stack of layers a read of `key` from `path` consults, per leaf:
    /// a tree shaped like the value with an ordered array at each leaf.
    /// Warms nothing and signals no demand. CLI-only (`comb status --tree`):
    /// no SDK binds it.
    Layers {
        key: String,
        #[serde(default)]
        path: Option<String>,
    },
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
    /// The template half of a `get`: the node at the requested address in the
    /// `template` facet of the same namespace. A JSON tree whose string leaves
    /// are value expressions the *client* renders, and whose objects are
    /// groups. Absent when the namespace has no template at that address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<serde_json::Value>,
    /// The unused remainder of the key when the template walk stopped on a
    /// string with segments left over — the address to apply to whatever that
    /// expression renders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest: Option<String>,
}

impl Response {
    pub fn ok(data: serde_json::Value, age_ms: u128, stale: bool) -> Self {
        Self {
            ok: true,
            data: Some(data),
            age_ms: Some(age_ms),
            stale: Some(stale),
            error: None,
            template: None,
            rest: None,
        }
    }

    pub fn miss() -> Self {
        Self {
            ok: true,
            data: None,
            age_ms: None,
            stale: None,
            error: None,
            template: None,
            rest: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            age_ms: None,
            stale: None,
            error: Some(msg.into()),
            template: None,
            rest: None,
        }
    }

    /// Attach the template half. Both are `None` on a node with no template,
    /// and `rest` is set only when the walk stopped on a string with segments
    /// left over.
    pub fn with_template(
        mut self,
        template: Option<serde_json::Value>,
        rest: Option<String>,
    ) -> Self {
        self.template = template;
        self.rest = rest;
        self
    }
}
