//! The wire: the requests the daemon takes, the response every op answers
//! with, and the payloads `status`, `introspect` and `hello` carry.
//!
//! One definition. The daemon deserialises requests and serialises responses
//! with these types (its `protocol` module re-exports them); this crate's
//! clients build the same requests and parse the same responses; the C ABI
//! hands the payloads to the SDKs as these types serialise. A request field
//! an op does not take is refused. A response field a client does not know
//! is ignored, so a newer daemon may add one.

use serde::{Deserialize, Serialize};

/// Wire protocol version. Semver: minor = additive, major = breaking.
/// See `docs/protocol-spec.md` for what counts as additive vs breaking.
pub const PROTOCOL_VERSION: &str = "1.0";

fn is_false(b: &bool) -> bool {
    !*b
}

/// One request line. A field the op does not take is refused, never ignored:
/// a retired or misspelt field is an error the client hears.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
pub enum Request {
    Get {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        force: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        wait: bool,
    },
    Refresh {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// An empty struct variant, not a unit one, so a field it does not take
    /// is refused like any other op's (`deny_unknown_fields` passes unit
    /// variants by).
    Status {},
    Put {
        key: String,
        data: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    Watch {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    Introspect {
        subject: IntrospectSubject,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_secs: Option<u64>,
    },
    /// The daemon's protocol version and build version, so a client can
    /// verify compatibility.
    Hello {},
    /// The stack of layers a read of `key` from `path` consults, per leaf:
    /// a tree shaped like the value with an ordered array at each leaf.
    /// Warms nothing and signals no demand. A diagnostic for
    /// `comb status --tree`: no SDK binds it.
    Layers {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
}

/// What an `introspect` request asks about. See `docs/protocol-spec.md` for
/// each subject's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

impl IntrospectSubject {
    /// Every subject, in the order `comb check` reports them.
    pub const ALL: [IntrospectSubject; 9] = [
        IntrospectSubject::Daemon,
        IntrospectSubject::Providers,
        IntrospectSubject::Config,
        IntrospectSubject::Cache,
        IntrospectSubject::Lifecycle,
        IntrospectSubject::Watches,
        IntrospectSubject::Timers,
        IntrospectSubject::Demand,
        IntrospectSubject::Procs,
    ];

    /// The subject's wire name: `daemon`, `providers`, …
    pub fn name(self) -> &'static str {
        match self {
            IntrospectSubject::Daemon => "daemon",
            IntrospectSubject::Providers => "providers",
            IntrospectSubject::Config => "config",
            IntrospectSubject::Cache => "cache",
            IntrospectSubject::Lifecycle => "lifecycle",
            IntrospectSubject::Watches => "watches",
            IntrospectSubject::Timers => "timers",
            IntrospectSubject::Demand => "demand",
            IntrospectSubject::Procs => "procs",
        }
    }
}

/// A subject by its wire name; anything else is `unknown introspect subject`.
impl std::str::FromStr for IntrospectSubject {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, String> {
        IntrospectSubject::ALL
            .into_iter()
            .find(|s| s.name() == name)
            .ok_or_else(|| format!("unknown introspect subject: {name}"))
    }
}

/// One response line, for every op and every watch frame. Fields with no
/// value are omitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_ms: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
            data: Some(data),
            age_ms: Some(age_ms),
            stale: Some(stale),
            ..Self::miss()
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
            error: Some(msg.into()),
            ..Self::miss()
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

/// The `hello` payload: the daemon's protocol and build versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloInfo {
    pub protocol_version: String,
    pub daemon_version: String,
}

/// One row of the `status` payload — one per (provider, path, source, field).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheRow {
    pub provider: String,
    /// The directory the row is stored at; `null` for the global entry.
    pub path: Option<String>,
    /// What produced this field. Inside the daemon it is the stored label
    /// (`head`, `put`, `config:<file>`), which keys the per-row lifecycle
    /// snapshot lookup so the renderer can format glyphs and TTL columns
    /// according to the owning Source's strategy, not the provider's. The
    /// `status` response presents it (`source(head)`, `put`,
    /// `config(<file>)`), and a row `comb status` renders itself says
    /// `rendered`.
    pub source: String,
    pub field: String,
    pub value: serde_json::Value,
    /// How old the value is. Every stored record has an age; a row
    /// `comb status` renders is as old as the oldest cached input behind it,
    /// and has none when it read no cached value at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_ms: Option<u128>,
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<RowKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_interval_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fsevents_reinstate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polls_elapsed: Option<u32>,
    /// Seconds until this source's next scheduled poll. `None` for pure Watch
    /// sources (no poll path) or when the lifecycle registry has no entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_poll_in_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureSnapshot>,
    /// A record the read from the caller's directory does not take: another
    /// layer of the same leaf answers, or the read answers nothing there.
    /// `comb status` works this out for itself (the daemon never sets it), and
    /// hides such rows unless asked (`--filter shadowed=true`, `-a`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shadowed: bool,
}

/// How a row's entry lives: the status formatter's choice of rendering.
/// `Lifecycle` entries get TTL countdown rendering; others render `---`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RowKind {
    Lifecycle {
        /// 0 = Active, 1..=`depth` = Decay step.
        decay: u8,
        /// The entry's own decay depth: steps from Active to eviction.
        depth: u8,
        watches_files: bool,
    },
    Once,
    Static,
    Transient,
    /// A row the CLI rendered from a template leaf for the caller's own
    /// directory and environment. Produced by the CLI, never by the daemon:
    /// a template is data the daemon holds at `<p>:template` and never
    /// evaluates, so `list_rows` never emits this kind and nothing carrying
    /// it comes off the wire.
    Rendered,
}

impl RowKind {
    /// The kind as `status` shows it: its serialised name.
    pub fn name(&self) -> &'static str {
        match self {
            RowKind::Lifecycle { .. } => "lifecycle",
            RowKind::Once => "once",
            RowKind::Static => "static",
            RowKind::Transient => "transient",
            RowKind::Rendered => "rendered",
        }
    }
}

/// Failure state for a cache entry, embedded in status rows: how many
/// consecutive execution failures have occurred and whether the provider is
/// currently suppressed (back-off in effect).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureSnapshot {
    pub consecutive_failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppressed_until_unix_ms: Option<u64>,
}

/// The `introspect` `daemon` payload: the daemon's own health.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonHealth {
    pub pid: i64,
    pub version: String,
    pub uptime_secs: u64,
    pub socket_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    pub requests_total: u64,
    pub in_flight: u64,
    pub active_watchers: u64,
    pub cache_entries: u64,
    /// `native`, `polling`, `disabled` or `unknown`.
    pub watch_backend: String,
    /// The reaper's capability; absent on an embedded or test server, which
    /// has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reaper: Option<ReaperStatus>,
    pub verdicts: Vec<Verdict>,
}

/// The reaper's capability, in [`DaemonHealth`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReaperStatus {
    pub armed: bool,
    /// `system-wide` or `confined`.
    pub visibility: String,
    pub sweeps: u64,
    pub reaped: u64,
    pub kill_denied: u64,
}

/// One health verdict: `PASS`, `WARN` or `FAIL`, and what it found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub level: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request serialises as the daemon reads it: absent options omitted,
    /// and every op round-trips.
    #[test]
    fn a_request_is_written_as_it_is_read() {
        let get = Request::Get {
            key: "git.branch".into(),
            path: None,
            force: false,
            wait: false,
        };
        assert_eq!(
            serde_json::to_value(&get).unwrap(),
            serde_json::json!({"op": "get", "key": "git.branch"})
        );
        assert_eq!(
            serde_json::to_value(Request::Status {}).unwrap(),
            serde_json::json!({"op": "status"})
        );
        for request in [
            get,
            Request::Hello {},
            Request::Introspect {
                subject: IntrospectSubject::Procs,
                duration_secs: Some(1),
            },
            Request::Put {
                key: "p".into(),
                data: serde_json::json!({"x": 1}),
                ttl: Some("1m".into()),
                path: Some("/r".into()),
            },
        ] {
            let line = serde_json::to_string(&request).unwrap();
            assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), request);
        }
    }

    #[test]
    fn a_subject_is_read_by_its_wire_name() {
        for subject in IntrospectSubject::ALL {
            assert_eq!(subject.name().parse(), Ok(subject));
            assert_eq!(
                serde_json::to_value(subject).unwrap(),
                serde_json::json!(subject.name())
            );
        }
        assert_eq!(
            "nope".parse::<IntrospectSubject>(),
            Err("unknown introspect subject: nope".to_string())
        );
    }
}
