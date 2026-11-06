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
use std::time::Duration;

/// Wire protocol version. Semver: minor = additive, major = breaking.
/// See `docs/protocol-spec.md` for what counts as additive vs breaking.
pub const PROTOCOL_VERSION: &str = "1.0";

fn is_false(b: &bool) -> bool {
    !*b
}

/// How long a `get` waits for a run it needs, past which it answers what is
/// stored and the run goes on and records itself.
///
/// It is written the same on every surface: `comb get --wait <DURATION|forever>`,
/// the wire's `"wait"`, the C ABI's options JSON and every SDK's `wait`, each
/// a duration in the one grammar ([`crate::duration`]) or `forever`. Leaving
/// it out is [`Wait::Budget`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Wait {
    /// The read's default: a stored record's Source's `budget`; a cold read
    /// and a `--force` wait for their run's end, which its `timeout` bounds.
    #[default]
    Budget,
    /// At most this long, counted from when the read began, whatever it finds.
    For(Duration),
    /// To the run's end.
    Forever,
}

impl Wait {
    pub fn is_budget(&self) -> bool {
        *self == Wait::Budget
    }

    /// How much longer than a client's own timeout an answer to a read that
    /// waits so may take: `None` is no deadline at all.
    pub fn beyond_timeout(self) -> Option<Duration> {
        match self {
            Wait::Budget => Some(Duration::ZERO),
            Wait::For(d) => Some(d),
            Wait::Forever => None,
        }
    }
}

/// `forever`, or a duration in the one grammar. The budget has no spelling:
/// it is a wait left out.
impl std::str::FromStr for Wait {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        if s == "forever" {
            return Ok(Wait::Forever);
        }
        crate::duration::parse(s).map(Wait::For).ok_or_else(|| {
            format!(
                "invalid wait `{s}`: `forever`, or a duration: {}",
                crate::duration::GRAMMAR
            )
        })
    }
}

/// A wait as the wire carries it: `forever`, or the duration in every unit it
/// has, so it reads back exactly. The budget is never written: a request
/// leaves it out.
impl Serialize for Wait {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Wait::Budget => serializer.serialize_none(),
            Wait::For(d) => serializer.serialize_str(&crate::render::duration_with(
                *d,
                crate::duration::Unit::ALL.len(),
                crate::duration::Unit::Milli,
            )),
            Wait::Forever => serializer.serialize_str("forever"),
        }
    }
}

impl<'de> Deserialize<'de> for Wait {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// A request's `wait`: `null` is absent, as everywhere, so it is the budget.
fn wait_or_absent<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Wait, D::Error> {
    Option::<Wait>::deserialize(deserializer).map(Option::unwrap_or_default)
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
        /// Absent (or `null`) is the budget.
        #[serde(
            default,
            deserialize_with = "wait_or_absent",
            skip_serializing_if = "Wait::is_budget"
        )]
        wait: Wait,
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
        /// Keep the put across a daemon restart, best effort.
        #[serde(default, skip_serializing_if = "is_false")]
        persist: bool,
    },
    Watch {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// Store what a template leaf's `| cache()` computed: the leaf's memo at
    /// `key` (a `provider.field`, never a facet) for the reader's `path`.
    /// `hash` is the sixteen lowercase hex digits the client computed over
    /// the leaf's inputs; `data` the leaf's final output, absent for a leaf
    /// that rendered nothing (a memo of absence); `watch` the absolute paths
    /// the leaf named; `ttl` its lifetime in whole seconds. The resolver
    /// writes it back after a render; no SDK binds it.
    Memo {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        hash: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        watch: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl: Option<String>,
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

impl Request {
    /// How much longer than a client's own timeout the daemon may take to
    /// answer: a `get`'s [`Wait`] ([`Wait::beyond_timeout`]), an `introspect`'s
    /// sample; every other op is answered within the timeout.
    pub fn beyond_timeout(&self) -> Option<Duration> {
        match self {
            Request::Get { wait, .. } => wait.beyond_timeout(),
            Request::Introspect {
                duration_secs: Some(secs),
                ..
            } => Some(Duration::from_secs(*secs)),
            _ => Some(Duration::ZERO),
        }
    }
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
    /// The memo half of a `get`, present only beside a template half: what
    /// `| cache()` stored for the template's leaves, read from the requested
    /// path up the chain, nearest first. For a string template one object,
    /// `{"hash", "stale", "age_ms", "data"}` (`hash` absent once an
    /// invalidation dropped it, `data` absent for a memo of absence); for a
    /// group a tree shaped like the template, that object at each leaf that
    /// has a memo. Absent when no leaf has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memo: Option<serde_json::Value>,
}

/// A `get`'s memo half with each memo's `age_ms` taken out, walked beside the
/// `template` half it was built from: a string template's half is one memo, a
/// group's a tree of them, so a key of a group is never mistaken for one.
/// What a watch compares a frame's memo half by, on both sides of the socket:
/// a dropped hash is a change, a memo that only grew older is not.
pub fn memo_without_ages(
    template: Option<&serde_json::Value>,
    memo: &serde_json::Value,
) -> serde_json::Value {
    match (template, memo) {
        (Some(serde_json::Value::Object(group)), serde_json::Value::Object(tree)) => tree
            .iter()
            .map(|(k, v)| (k.clone(), memo_without_ages(group.get(k), v)))
            .collect::<serde_json::Map<_, _>>()
            .into(),
        (_, serde_json::Value::Object(leaf)) => {
            let mut leaf = leaf.clone();
            leaf.remove("age_ms");
            leaf.into()
        }
        (_, other) => other.clone(),
    }
}

impl Response {
    pub fn ok(data: serde_json::Value, age_ms: u128) -> Self {
        Self {
            data: Some(data),
            age_ms: Some(age_ms),
            ..Self::miss()
        }
    }

    pub fn miss() -> Self {
        Self {
            ok: true,
            data: None,
            age_ms: None,
            error: None,
            template: None,
            rest: None,
            memo: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            ..Self::miss()
        }
    }

    /// Attach the template half and the memo half beside it. All three are
    /// `None` on a node with no template; `rest` is set only when the walk
    /// stopped on a string with segments left over, and `memo` only when a
    /// leaf of the template has a memo.
    pub fn with_halves(
        mut self,
        template: Option<serde_json::Value>,
        rest: Option<String>,
        memo: Option<serde_json::Value>,
    ) -> Self {
        self.template = template;
        self.rest = rest;
        self.memo = memo;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<RowKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_interval_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_count: Option<u32>,
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
/// consecutive execution failures have occurred, whether the provider is
/// currently suppressed (back-off in effect), and what the latest one said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureSnapshot {
    pub consecutive_failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppressed_until_unix_ms: Option<u64>,
    /// The latest failure's message: its first line, cut short.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
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
            wait: Wait::Budget,
        };
        assert_eq!(
            serde_json::to_value(&get).unwrap(),
            serde_json::json!({"op": "get", "key": "git.branch"})
        );
        assert_eq!(
            serde_json::to_value(Request::Status {}).unwrap(),
            serde_json::json!({"op": "status"})
        );
        // A memo of absence with no triggers is the key, the path and the
        // hash alone.
        assert_eq!(
            serde_json::to_value(Request::Memo {
                key: "p.f".into(),
                path: None,
                hash: "0000000000000000".into(),
                data: None,
                watch: Vec::new(),
                ttl: None,
            })
            .unwrap(),
            serde_json::json!({"op": "memo", "key": "p.f", "hash": "0000000000000000"})
        );
        let waits = |wait| Request::Get {
            key: "git".into(),
            path: Some("/r".into()),
            force: true,
            wait,
        };
        for request in [
            get,
            waits(Wait::For(Duration::ZERO)),
            waits(Wait::For(Duration::from_millis(50))),
            waits(Wait::For(Duration::from_millis(90_061_001))),
            waits(Wait::Forever),
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
                persist: true,
            },
            Request::Memo {
                key: "kubecontext.context".into(),
                path: Some("/r".into()),
                hash: "9c6001d328a94690".into(),
                data: Some(serde_json::json!("prod")),
                watch: vec!["/h/.kube/config".into()],
                ttl: Some("10m".into()),
            },
        ] {
            let line = serde_json::to_string(&request).unwrap();
            assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), request);
        }
    }

    /// A `get`'s wait is a duration or `forever`, written so it reads back.
    /// The boolean it replaced, and anything else, is an invalid request.
    #[test]
    fn a_wait_is_a_duration_or_forever() {
        let wait = |line: &str| match serde_json::from_str::<Request>(line) {
            Ok(Request::Get { wait, .. }) => Ok(wait),
            Ok(other) => panic!("not a get: {other:?}"),
            Err(e) => Err(e.to_string()),
        };
        assert_eq!(wait(r#"{"op":"get","key":"k"}"#), Ok(Wait::Budget));
        assert_eq!(
            wait(r#"{"op":"get","key":"k","wait":"0"}"#),
            Ok(Wait::For(Duration::ZERO))
        );
        assert_eq!(
            wait(r#"{"op":"get","key":"k","wait":"1m30s"}"#),
            Ok(Wait::For(Duration::from_secs(90)))
        );
        assert_eq!(
            wait(r#"{"op":"get","key":"k","wait":"forever"}"#),
            Ok(Wait::Forever)
        );
        assert_eq!(
            wait(r#"{"op":"get","key":"k","wait":null}"#),
            Ok(Wait::Budget),
            "null is absent"
        );
        for refused in ["true", "false", "50", r#""soon""#, r#""""#, r#""budget""#] {
            let line = format!(r#"{{"op":"get","key":"k","wait":{refused}}}"#);
            assert!(wait(&line).is_err(), "{line} must be refused");
        }
        assert!(
            wait(r#"{"op":"get","key":"k","wait":"soon"}"#)
                .unwrap_err()
                .contains("invalid wait `soon`: `forever`, or a duration"),
        );
        assert_eq!(
            serde_json::to_value(Request::Get {
                key: "k".into(),
                path: None,
                force: false,
                wait: Wait::For(Duration::from_millis(1500)),
            })
            .unwrap()["wait"],
            serde_json::json!("1s500ms")
        );
    }

    /// A client's read of a `get` may take its timeout plus the wait;
    /// `forever` has no deadline, and every other op has the timeout alone.
    #[test]
    fn a_read_waits_past_the_timeout_by_its_wait() {
        let get = |wait| Request::Get {
            key: "k".into(),
            path: None,
            force: false,
            wait,
        };
        assert_eq!(get(Wait::Budget).beyond_timeout(), Some(Duration::ZERO));
        assert_eq!(
            get(Wait::For(Duration::from_secs(2))).beyond_timeout(),
            Some(Duration::from_secs(2))
        );
        assert_eq!(get(Wait::Forever).beyond_timeout(), None);
        assert_eq!(Request::Status {}.beyond_timeout(), Some(Duration::ZERO));
        let sample = Request::Introspect {
            subject: IntrospectSubject::Procs,
            duration_secs: Some(90),
        };
        assert_eq!(sample.beyond_timeout(), Some(Duration::from_secs(90)));
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
