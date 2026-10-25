//! # libbeachcomber
//!
//! A lightweight, synchronous client for the beachcomber (`comb`) shell state daemon.
//!
//! ```rust,no_run
//! use libbeachcomber::{Client, CombResult};
//!
//! let client = Client::new();
//! match client.get("git.branch", Some("/path/to/repo")) {
//!     Ok(CombResult::Hit { data, age_ms, stale, .. }) => {
//!         println!("branch: {}", data.get_str("git.branch").unwrap_or("?"));
//!     }
//!     Ok(CombResult::Template { template, .. }) => {
//!         println!("a template, not a value: {template} — render it with `resolve`");
//!     }
//!     Ok(CombResult::Miss) => println!("not cached yet"),
//!     Err(e) => println!("error: {}", e),
//! }
//! ```

pub mod engine;
pub mod eval;
// `exec()` spawn engine, backing the `exec()` jinja function registered in
// `engine::build_engine_env`.
mod exec;
pub mod filters;
pub mod key;
pub mod render;
pub mod resolve;
pub mod virtual_fields;
pub mod watch;

/// This crate's build identity, derived by `build.rs` from `CARGO_PKG_VERSION`
/// plus git sha/dirty state. Shares its derivation with the root crate's
/// `BEACHCOMBER_VERSION` (see `build-common/version.rs`) so the two always
/// report the same string.
pub const VERSION: &str = env!("BEACHCOMBER_VERSION");

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Connect to a Unix socket with 3 retries (250ms / 500ms / 1s exponential backoff).
///
/// Retries on `ConnectionRefused` and `NotFound` only — other errors surface
/// immediately. Intended to cover the brief restart window when the old daemon
/// has shut down and the new one hasn't bound yet.
pub fn connect_with_retry(path: &Path) -> std::io::Result<UnixStream> {
    connect_with_retry_for(path, DEFAULT_CONNECT_TIMEOUT)
}

/// Total time [`connect_with_retry`] keeps trying before giving up.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);

/// Like [`connect_with_retry`], but keeps trying for up to `budget`. The
/// first retry waits 5ms and each later one doubles (5, 10, 20, 40, ... ms,
/// the last clipped to the budget), so a daemon that binds early is reached
/// within milliseconds while a restart window as long as `budget` is still
/// covered. A zero budget is a single attempt.
pub fn connect_with_retry_for(path: &Path, budget: Duration) -> std::io::Result<UnixStream> {
    let deadline = Instant::now() + budget;
    let mut delay = Duration::from_millis(5);
    loop {
        match UnixStream::connect(path) {
            Ok(s) => return Ok(s),
            Err(e) => {
                if !matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) {
                    return Err(e);
                }
                let now = Instant::now();
                if now >= deadline {
                    return Err(e);
                }
                std::thread::sleep(delay.min(deadline - now));
                delay *= 2;
            }
        }
    }
}

/// What a raw `get` found at an address: the daemon's answer, classified.
///
/// A node may hold a value (`data`), a template (`template`), or both. The
/// value is the cached half; the template is the half the *client* renders,
/// because rendering needs the caller's environment and directory. A raw
/// `get` hands both back as they arrived — [`resolve`] is what renders the
/// template into the value `comb get` prints.
#[derive(Debug, PartialEq)]
pub enum CombResult {
    /// The node holds a value. It may also hold a template, which wins on
    /// read when rendered (`resolve`); a raw `get` hands both back.
    Hit {
        data: CombData,
        age_ms: u128,
        stale: bool,
        /// The template in effect at the address, if any: a string for a
        /// leaf, an object tree for a group.
        template: Option<serde_json::Value>,
        /// The address left over when the template walk stopped at a string.
        rest: Option<String>,
    },
    /// The node holds only a template: an address whose value is rendered
    /// client-side (`resolve`). Not a miss — something is stored there — and
    /// not a hit — there is no value until it is rendered.
    Template {
        /// A string for a leaf, an object tree for a group.
        template: serde_json::Value,
        /// The address left over when the template walk stopped at a string.
        rest: Option<String>,
    },
    /// Nothing is stored at the address.
    Miss,
}

impl CombResult {
    /// True for [`Hit`](Self::Hit) only: the node holds a value.
    pub fn is_hit(&self) -> bool {
        matches!(self, CombResult::Hit { .. })
    }

    /// True for [`Miss`](Self::Miss) only. A [`Template`](Self::Template) is
    /// not a miss: render it with [`resolve`].
    pub fn is_miss(&self) -> bool {
        matches!(self, CombResult::Miss)
    }

    /// The template half, from a `Hit` that carries one or a `Template`.
    pub fn template(&self) -> Option<&serde_json::Value> {
        match self {
            CombResult::Hit { template, .. } => template.as_ref(),
            CombResult::Template { template, .. } => Some(template),
            CombResult::Miss => None,
        }
    }

    /// The address left over past a template string, from either variant.
    pub fn rest(&self) -> Option<&str> {
        match self {
            CombResult::Hit { rest, .. } | CombResult::Template { rest, .. } => rest.as_deref(),
            CombResult::Miss => None,
        }
    }
}

/// Classify a node: `data` present is a `Hit` (the template half rides
/// along), a template alone is a `Template`, neither is a `Miss`. `age_ms`
/// and `stale` always accompany `data` on the wire; the defaults are never
/// reached. `rest` never arrives without `template` — the daemon sets it only
/// when the template walk stopped at a string — so a `Miss` ignores it.
impl From<resolve::Got> for CombResult {
    fn from(got: resolve::Got) -> Self {
        match got {
            resolve::Got {
                data: Some(value),
                template,
                rest,
                age_ms,
                stale,
            } => CombResult::Hit {
                data: CombData { value },
                age_ms: age_ms.unwrap_or(0),
                stale: stale.unwrap_or(false),
                template,
                rest,
            },
            resolve::Got {
                data: None,
                template: Some(template),
                rest,
                ..
            } => CombResult::Template { template, rest },
            resolve::Got {
                data: None,
                template: None,
                ..
            } => CombResult::Miss,
        }
    }
}

/// Parsed response data from a provider.
#[derive(Debug, Clone, PartialEq)]
pub struct CombData {
    value: serde_json::Value,
}

impl CombData {
    /// Create from a raw JSON value (useful for testing).
    pub fn from_json(value: serde_json::Value) -> Self {
        Self { value }
    }

    /// Get a string field. For single-field queries (e.g., "git.branch"),
    /// this returns the value directly. For full provider queries (e.g., "git"),
    /// access fields by name.
    pub fn get_str(&self, field: &str) -> Option<&str> {
        if let Some(obj) = self.value.as_object() {
            obj.get(field).and_then(|v| v.as_str())
        } else {
            self.value.as_str()
        }
    }

    pub fn get_bool(&self, field: &str) -> Option<bool> {
        if let Some(obj) = self.value.as_object() {
            obj.get(field).and_then(|v| v.as_bool())
        } else {
            self.value.as_bool()
        }
    }

    pub fn get_i64(&self, field: &str) -> Option<i64> {
        if let Some(obj) = self.value.as_object() {
            obj.get(field).and_then(|v| v.as_i64())
        } else {
            self.value.as_i64()
        }
    }

    pub fn get_f64(&self, field: &str) -> Option<f64> {
        if let Some(obj) = self.value.as_object() {
            obj.get(field).and_then(|v| v.as_f64())
        } else {
            self.value.as_f64()
        }
    }

    /// Get the raw serde_json::Value.
    pub fn as_value(&self) -> &serde_json::Value {
        &self.value
    }

    /// Get as raw text (for single-field queries like "git.branch").
    pub fn as_text(&self) -> Option<String> {
        match &self.value {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::Bool(b) => Some(b.to_string()),
            serde_json::Value::Null => None,
            other => Some(other.to_string()),
        }
    }
}

/// Protocol and build version information returned by the daemon on `hello`.
#[derive(Debug, Clone)]
pub struct HelloInfo {
    pub protocol_version: String,
    pub daemon_version: String,
}

/// Reported when this client's build identity differs from the daemon's,
/// discovered via `hello` on the connection's first use. Not fatal — a
/// running older daemon is a normal state mid-upgrade — but callers can
/// check for it, and it is named in any op error on that connection
/// afterward (see `CombError::ServerError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionSkew {
    /// This client's build identity (`VERSION`).
    pub ours: String,
    /// The daemon's reported `daemon_version`.
    pub theirs: String,
}

impl std::fmt::Display for VersionSkew {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "client is {}, daemon is {}", self.ours, self.theirs)
    }
}

/// Compare a `hello` response against this build's identity.
fn detect_skew(hello: &HelloInfo) -> Option<VersionSkew> {
    if hello.daemon_version == VERSION {
        None
    } else {
        Some(VersionSkew {
            ours: VERSION.to_string(),
            theirs: hello.daemon_version.clone(),
        })
    }
}

/// Send a `hello` request on `stream` and compare the daemon's reported
/// version against this build's. Used once per connection, at the point
/// each connection is established, so the check costs one extra
/// request/response round trip rather than one per op.
fn probe_version_skew(stream: &mut UnixStream) -> Result<Option<VersionSkew>, CombError> {
    let request = serde_json::json!({ "op": "hello" });
    let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
    stream.write_all(msg.as_bytes())?;

    let mut line = String::new();
    BufReader::new(&mut *stream).read_line(&mut line)?;
    let hello = parse_hello_response(&line)?;
    Ok(detect_skew(&hello))
}

/// Discriminator used by the status formatter to choose rendering strategy.
/// Mirrors `beachcomber::cache::RowKind` for the wire format.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RowKind {
    Lifecycle {
        decay: u8,
        depth: u8,
        watches_files: bool,
    },
    Once,
    Static,
    Transient,
}

/// Failure state for a cache entry embedded in status rows.
/// Mirrors `beachcomber::cache::FailureSnapshot` for the wire format.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FailureSnapshot {
    pub consecutive_failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppressed_until_unix_ms: Option<u64>,
}

/// One row of the daemon's cache as returned by the `status` op.
#[derive(Debug, Clone)]
pub struct CacheRow {
    pub provider: String,
    pub field: Option<String>,
    pub path: Option<String>,
    pub value: serde_json::Value,
    pub age_ms: u64,
    pub stale: bool,
    /// Phase 2.7: lifecycle classification of this cache entry.
    pub kind: Option<RowKind>,
    /// Phase 2.7: how often the provider is polled, in seconds.
    pub poll_interval_secs: Option<u64>,
    /// Phase 2.7: number of polls before a demanded key decays.
    pub poll_count: Option<u32>,
    /// Phase 2.7: whether FSEvents will reinstate watching after a miss.
    pub fsevents_reinstate: Option<bool>,
    /// Number of polls that have fired in the current lifecycle step.
    pub polls_elapsed: Option<u32>,
    /// Seconds until this source's next scheduled poll.
    pub next_poll_in_secs: Option<u64>,
    /// Phase 2.7: failure state if the provider has been failing.
    pub failure: Option<FailureSnapshot>,
    /// Phase 5: source name within the provider that owns this field.
    pub source: Option<String>,
}

impl CacheRow {
    /// Parse a `CacheRow` from the daemon's wire-format JSON object.
    /// Unknown fields are silently ignored.
    pub fn from_wire(v: &serde_json::Value) -> Result<Self, CombError> {
        let provider = v
            .get("provider")
            .and_then(|x| x.as_str())
            .ok_or_else(|| CombError::ParseError("cache row missing provider".into()))?
            .to_string();
        let field = v
            .get("field")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        let path = v
            .get("path")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        let value = v.get("value").cloned().unwrap_or(serde_json::Value::Null);
        let age_ms = v.get("age_ms").and_then(|x| x.as_u64()).unwrap_or(0);
        let stale = v.get("stale").and_then(|x| x.as_bool()).unwrap_or(false);
        let kind = v
            .get("kind")
            .and_then(|x| serde_json::from_value(x.clone()).ok());
        let poll_interval_secs = v.get("poll_interval_secs").and_then(|x| x.as_u64());
        let poll_count = v
            .get("poll_count")
            .and_then(|x| x.as_u64().map(|n| n as u32));
        let fsevents_reinstate = v.get("fsevents_reinstate").and_then(|x| x.as_bool());
        let polls_elapsed = v
            .get("polls_elapsed")
            .and_then(|x| x.as_u64().map(|n| n as u32));
        let next_poll_in_secs = v.get("next_poll_in_secs").and_then(|x| x.as_u64());
        let failure = v
            .get("failure")
            .and_then(|x| serde_json::from_value(x.clone()).ok());
        let source = v
            .get("source")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        Ok(CacheRow {
            provider,
            field,
            path,
            value,
            age_ms,
            stale,
            kind,
            poll_interval_secs,
            poll_count,
            fsevents_reinstate,
            polls_elapsed,
            next_poll_in_secs,
            failure,
            source,
        })
    }

    fn from_json(v: &serde_json::Value) -> Result<Self, CombError> {
        Self::from_wire(v)
    }
}

#[derive(Debug, Clone)]
pub struct Verdict {
    pub level: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct DaemonHealth {
    pub pid: i64,
    pub version: String,
    pub uptime_secs: u64,
    pub socket_path: String,
    pub config_path: Option<String>,
    pub requests_total: u64,
    pub in_flight: u64,
    pub active_watchers: u64,
    pub cache_entries: u64,
    /// "native", "polling", "disabled", or "unknown"; absent from pre-0.8 daemons.
    pub watch_backend: Option<String>,
    /// Reaper capability snapshot (canon `singleton.md` invariant 13).
    /// `None` when reaper health isn't attached (embedded/test servers).
    pub reaper: Option<ReaperStatus>,
    pub verdicts: Vec<Verdict>,
}

/// Reaper capability snapshot embedded in `introspect{daemon}`.
/// Mirrors the `reaper_json` shape built by `server.rs`.
#[derive(Debug, Clone)]
pub struct ReaperStatus {
    pub armed: bool,
    /// "system-wide" or "confined".
    pub visibility: String,
    pub sweeps: u64,
    pub reaped: u64,
    pub kill_denied: u64,
}

/// Introspect subjects. See `docs/protocol-spec.md` for shape details.
#[derive(Debug, Clone, Copy)]
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
    fn wire_name(&self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Providers => "providers",
            Self::Config => "config",
            Self::Cache => "cache",
            Self::Lifecycle => "lifecycle",
            Self::Watches => "watches",
            Self::Timers => "timers",
            Self::Demand => "demand",
            Self::Procs => "procs",
        }
    }
}

/// Introspect response. Daemon subject is typed as DaemonHealth;
/// other subjects are returned as raw JSON pending per-subject typing
/// in later phases.
#[derive(Debug, Clone)]
pub enum IntrospectResponse {
    Daemon(DaemonHealth),
    Other(serde_json::Value),
}

/// A single event emitted by the daemon on a watched key.
#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub data: Option<CombData>,
    pub age_ms: u64,
    pub stale: bool,
}

/// Streaming iterator over watch events. Each `next_event` call blocks
/// until the daemon emits the next change (or the connection closes).
///
/// The underlying connection is held open for the lifetime of this
/// stream; drop it to disconnect.
pub struct WatchStream {
    reader: BufReader<UnixStream>,
    /// The bytes of a frame that arrived split across two bounded reads, kept
    /// so [`WatchStream::read_frame_with_timeout`] resumes mid-line rather
    /// than losing half an event. Bytes, not a `String`: a frame can split
    /// mid-UTF-8-codepoint at a tick boundary, and only a complete line is
    /// ever decoded, so a `String` buffer would force decoding (and
    /// discarding) partial, possibly-invalid bytes on every call.
    pending: Vec<u8>,
}

impl WatchStream {
    /// Read the next watch event. Returns Ok(None) on connection close.
    pub fn next_event(&mut self) -> Result<Option<WatchEvent>, CombError> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        Self::parse_line(&line).map(Some)
    }

    /// Parse one NDJSON watch-event line into a [`WatchEvent`]. Factored out
    /// of [`Self::next_event`] so the fan-in
    /// ([`crate::watch::ResolvedWatch`]), which reads frames itself through
    /// `read_frame_with_timeout` rather than through `next_event`,
    /// reuses the same parsing instead of duplicating it.
    pub fn parse_line(line: &str) -> Result<WatchEvent, CombError> {
        let resp: serde_json::Value =
            serde_json::from_str(line.trim()).map_err(|e| CombError::ParseError(e.to_string()))?;
        let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        if !ok {
            let error = resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error")
                .to_string();
            return Err(CombError::ServerError(error));
        }
        let data = match resp.get("data") {
            Some(serde_json::Value::Null) | None => None,
            Some(d) => Some(CombData::from_json(d.clone())),
        };
        let age_ms = resp.get("age_ms").and_then(|v| v.as_u64()).unwrap_or(0);
        let stale = resp.get("stale").and_then(|v| v.as_bool()).unwrap_or(false);
        Ok(WatchEvent {
            data,
            age_ms,
            stale,
        })
    }

    /// Sets the underlying socket's read timeout. `None` blocks
    /// indefinitely. `read_frame_with_timeout` is the bounded read
    /// built on it, and is what every poll loop in the workspace uses —
    /// there is deliberately no raw line-reading escape hatch beside it for
    /// a caller to rebuild that loop from.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.reader.get_ref().set_read_timeout(timeout)
    }

    /// Read one whole frame, waiting at most `d` for it.
    ///
    /// The three outcomes a fan-in polling several streams in turn has to
    /// tell apart ([`crate::watch::ResolvedWatch`]): `Ok(Some(line))` is a
    /// frame, `Ok(None)` the daemon closing the connection, and an `Err`
    /// whose `kind()` is `WouldBlock` or `TimedOut` the wait expiring with
    /// nothing to read. Buffers bytes in `pending` rather than a `String`, so
    /// a frame split across two waits — even mid-UTF-8-codepoint — resumes
    /// rather than being dropped; a complete line that turns out not to be
    /// valid UTF-8 is an `Err` of kind `InvalidData`, not a silent drop.
    pub(crate) fn read_frame_with_timeout(
        &mut self,
        d: Duration,
    ) -> std::io::Result<Option<String>> {
        // A socket that has just lost its peer can fail the timeout call
        // itself (macOS `EINVAL` on an abruptly reset unix socket) rather
        // than surfacing the loss on the read. There is no connection left to
        // wait on either way, which is what `Ok(None)` says.
        if self.set_read_timeout(Some(d)).is_err() {
            return Ok(None);
        }
        match self.reader.read_until(b'\n', &mut self.pending) {
            Ok(0) => Ok(None),
            Ok(_) if self.pending.ends_with(b"\n") => {
                let line = std::mem::take(&mut self.pending);
                String::from_utf8(line)
                    .map(Some)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            }
            // `read_until` stops short of a newline only at end of input: a
            // partial last frame, which is no frame at all.
            Ok(_) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Error type for client operations.
#[derive(Debug)]
pub enum CombError {
    /// Daemon is not running and could not be started.
    DaemonNotRunning,
    /// Socket connection failed.
    ConnectionFailed(std::io::Error),
    /// Request/response I/O failed.
    IoError(std::io::Error),
    /// Response couldn't be parsed.
    ParseError(String),
    /// Server returned an error.
    ServerError(String),
    /// Operation timed out.
    Timeout,
}

impl std::fmt::Display for CombError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CombError::DaemonNotRunning => write!(f, "comb daemon is not running"),
            CombError::ConnectionFailed(e) => write!(f, "connection failed: {}", e),
            CombError::IoError(e) => write!(f, "I/O error: {}", e),
            CombError::ParseError(s) => write!(f, "parse error: {}", s),
            CombError::ServerError(s) => write!(f, "server error: {}", s),
            CombError::Timeout => write!(f, "operation timed out"),
        }
    }
}

impl std::error::Error for CombError {}

impl From<std::io::Error> for CombError {
    fn from(e: std::io::Error) -> Self {
        CombError::IoError(e)
    }
}

/// Configuration for the client.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Read/write timeout for socket operations.
    pub timeout: Duration,
    /// Whether to attempt starting the daemon if it's not running.
    pub auto_start: bool,
    /// Whether templates this client resolves may call `exec()`. Off by
    /// default: an embedding caller does not spawn processes on the user's
    /// behalf unless it says so (canon `field_resolution.md` invariant 15).
    pub exec: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_millis(100),
            auto_start: true,
            exec: false,
        }
    }
}

/// A synchronous client for the beachcomber daemon.
///
/// Each method call creates a new socket connection. For multiple
/// queries in sequence, use [`Session`] instead.
pub struct Client {
    config: ClientConfig,
    socket_path_override: Option<PathBuf>,
    /// How long `connect` keeps retrying before giving up.
    connect_timeout: Duration,
    /// Version skew, if any, discovered via `hello` on this client's first
    /// connection. Checked once — later connections reuse this rather than
    /// probing again, so it is never a per-op cost.
    version_skew: OnceLock<Option<VersionSkew>>,
}

impl Client {
    /// Create a client with default configuration (100ms timeout, auto-start enabled).
    pub fn new() -> Self {
        Self {
            config: ClientConfig::default(),
            socket_path_override: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            version_skew: OnceLock::new(),
        }
    }

    /// Create a client with custom configuration.
    pub fn with_config(config: ClientConfig) -> Self {
        Self {
            config,
            socket_path_override: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            version_skew: OnceLock::new(),
        }
    }

    /// Version skew detected against the daemon, if any. `None` before the
    /// first connection is made, or once made, if the versions matched.
    pub fn version_skew(&self) -> Option<VersionSkew> {
        self.version_skew.get().cloned().flatten()
    }

    /// Whether templates this client resolves may call `exec()`
    /// ([`ClientConfig::exec`]).
    pub fn allow_exec(&self) -> bool {
        self.config.exec
    }

    fn skew_ref(&self) -> Option<&VersionSkew> {
        self.version_skew.get().and_then(|s| s.as_ref())
    }

    /// Probe `stream` for version skew if not already known, via a
    /// trailing `hello` exchange *after* the caller's own request/response
    /// on the connection — never before it. Checked once per `Client`, not
    /// per op. Probe failures are swallowed (skew stays unknown) so a
    /// connection that only ever answers the caller's own request is
    /// unaffected.
    fn probe_skew_after(&self, stream: &mut UnixStream) {
        if self.version_skew.get().is_none() {
            let skew = probe_version_skew(stream).unwrap_or(None);
            let _ = self.version_skew.set(skew);
        }
    }

    /// Override the socket path (bypassing auto-discovery). Primarily
    /// useful for tests that spawn a daemon on a custom socket.
    pub fn with_socket_path(mut self, path: PathBuf) -> Self {
        self.socket_path_override = Some(path);
        self
    }

    /// How long connecting keeps retrying before giving up (default
    /// [`DEFAULT_CONNECT_TIMEOUT`]). Zero means a single attempt.
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Query a single key, raw: the daemon's answer as it arrived, nothing
    /// rendered. A [`CombResult::Hit`] carries the value (and the template in
    /// effect there, if any); a [`CombResult::Template`] is a node that holds
    /// only a template — not a miss, but not a value until [`resolve`]
    /// renders it; a [`CombResult::Miss`] has nothing stored.
    ///
    /// Examples:
    /// - `client.get("git.branch", Some("/path/to/repo"))` — single field
    /// - `client.get("git", Some("/path/to/repo"))` — all fields
    /// - `client.get("hostname.short", None)` — global provider
    /// - `client.get("scanline.widgets.foo", None)` on a templated address —
    ///   `Template { template: "{{ env.SCANLINE_FOO }}", rest: None }`
    pub fn get(&self, key: &str, path: Option<&str>) -> Result<CombResult, CombError> {
        self.get_with_flags(key, path, false, false)
    }

    /// Query a key with optional flags.
    ///
    /// `force = true`: evict the cache entry and re-execute the provider before returning.
    /// `wait = true`: reserved for T14; currently a no-op.
    pub fn get_with_flags(
        &self,
        key: &str,
        path: Option<&str>,
        force: bool,
        wait: bool,
    ) -> Result<CombResult, CombError> {
        let socket_path = self.find_or_start_socket()?;
        let mut stream = self.connect(&socket_path)?;

        self.send_recv(&mut stream, &get_request(key, path, force, wait))
    }

    /// Query one node and return both halves of the daemon's answer — the
    /// cached `data` and the `template` in effect at the address — as the
    /// [`resolve`] module's [`Fetch`](resolve::Fetch) needs them. The same
    /// `get` request as [`get_with_flags`](Self::get_with_flags); a miss is
    /// `Got::default()`, a rejected key `CombError::ServerError`.
    pub fn get_node(
        &self,
        key: &str,
        path: Option<&str>,
        force: bool,
        wait: bool,
    ) -> Result<resolve::Got, CombError> {
        let socket_path = self.find_or_start_socket()?;
        let mut stream = self.connect(&socket_path)?;

        let line = self.send_recv_line(&mut stream, &get_request(key, path, force, wait))?;
        parse_node(&line, self.skew_ref())
    }

    /// Query a single key, rendered by the daemon as plain text.
    /// Shorthand for `get_formatted(key, path, "text")`.
    pub fn get_text(&self, key: &str, path: Option<&str>) -> Result<String, CombError> {
        self.get_formatted(key, path, "text")
    }

    /// Query a single key, rendered server-side into `format` (e.g. `"text"`, `"sh"`).
    pub fn get_formatted(
        &self,
        key: &str,
        path: Option<&str>,
        format: &str,
    ) -> Result<String, CombError> {
        self.get_formatted_with_flags(key, path, format, false, false)
    }

    /// Query a single key with optional flags, rendered into `format`
    /// (`"text"` or `"sh"`) on the client side.
    ///
    /// `sh` shell-quotes every rendered value; `text` leaves them raw (see
    /// [`render::render_data`]). This performs a plain JSON `get` and renders
    /// the result locally (Task 1.8 retired the server-rendered wire
    /// sub-protocol this used to speak); a failed request surfaces as
    /// `CombError::ServerError`, never as a rendered `error:` line. This is
    /// raw like [`get`](Self::get): a template-only node renders its template
    /// text, not the value [`resolve`] would make of it.
    pub fn get_formatted_with_flags(
        &self,
        key: &str,
        path: Option<&str>,
        format: &str,
        force: bool,
        wait: bool,
    ) -> Result<String, CombError> {
        match self.get_with_flags(key, path, force, wait)? {
            CombResult::Hit { data, .. } => Ok(render::render_data(Some(data.as_value()), format)),
            CombResult::Template { template, .. } => {
                Ok(render::render_data(Some(&template), format))
            }
            CombResult::Miss => Ok(render::render_data(None, format)),
        }
    }

    /// Store data at a key, replacing whatever is stored at that address.
    /// `key` addresses any node: `namespace` or `namespace.node…`. At the root
    /// `data` must be a JSON object; below it, any JSON.
    pub fn put(
        &self,
        key: &str,
        data: serde_json::Value,
        ttl: Option<&str>,
        path: Option<&str>,
    ) -> Result<(), CombError> {
        let socket_path = self.find_or_start_socket()?;
        let mut stream = self.connect(&socket_path)?;

        let mut request = serde_json::json!({
            "op": "put",
            "key": key,
            "data": data,
        });
        if let Some(t) = ttl {
            request["ttl"] = serde_json::json!(t);
        }
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        stream.write_all(msg.as_bytes())?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        self.probe_skew_after(reader.get_mut());
        check_ok(&line, self.skew_ref())?;
        Ok(())
    }

    /// Trigger recomputation of a provider. Fire-and-forget.
    pub fn refresh(&self, key: &str, path: Option<&str>) -> Result<(), CombError> {
        let socket_path = self.find_or_start_socket()?;
        let mut stream = self.connect(&socket_path)?;

        let mut request = serde_json::json!({ "op": "refresh", "key": key });
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }

        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        stream.write_all(msg.as_bytes())?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        self.probe_skew_after(reader.get_mut());
        check_ok(&line, self.skew_ref())?;
        Ok(())
    }

    /// List all cache entries currently held by the daemon.
    pub fn status(&self) -> Result<Vec<CacheRow>, CombError> {
        let socket_path = self.find_or_start_socket()?;
        let mut stream = self.connect(&socket_path)?;
        let request = serde_json::json!({ "op": "status" });
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        stream.write_all(msg.as_bytes())?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        self.probe_skew_after(reader.get_mut());
        parse_cache_rows(&line, self.skew_ref())
    }

    /// The layers a read of `key` from `path` consults, per leaf: a tree
    /// shaped like the value with an ordered array of
    /// `{label, facet?, path, value, age_ms, stale, answered?}` at each leaf
    /// (a leaf key answers with its array). `None` is a miss. Warms nothing.
    /// Used by `comb status --tree`; no SDK binds it.
    pub fn layers(
        &self,
        key: &str,
        path: Option<&str>,
    ) -> Result<Option<serde_json::Value>, CombError> {
        let socket_path = self.find_or_start_socket()?;
        let mut stream = self.connect(&socket_path)?;
        let line = self.send_recv_line(&mut stream, &layers_request(key, path))?;
        parse_layers(&line, self.skew_ref())
    }

    /// Run an introspect query. `duration_secs` is only consulted by the
    /// `procs` subject; ignored by others.
    pub fn introspect(
        &self,
        subject: IntrospectSubject,
        duration_secs: Option<u64>,
    ) -> Result<IntrospectResponse, CombError> {
        let socket_path = self.find_or_start_socket()?;
        let mut stream = self.connect(&socket_path)?;
        let mut request = serde_json::json!({
            "op": "introspect",
            "subject": subject.wire_name(),
        });
        if let Some(d) = duration_secs {
            request["duration_secs"] = serde_json::json!(d);
        }
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        stream.write_all(msg.as_bytes())?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        self.probe_skew_after(reader.get_mut());
        parse_introspect(subject, &line, self.skew_ref())
    }

    /// Subscribe to changes on a key. Returns a stream that blocks on
    /// `next_event` until the daemon emits a change (or the connection
    /// closes). The first event is always the current value.
    ///
    /// Watch is NOT available on Session because the daemon puts the
    /// connection into streaming mode once a watch is issued — no other
    /// ops can share that connection afterward.
    pub fn watch(&self, key: &str, path: Option<&str>) -> Result<WatchStream, CombError> {
        let socket_path = self.find_or_start_socket()?;
        let mut stream = self.connect(&socket_path)?;
        let mut request = serde_json::json!({ "op": "watch", "key": key });
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        stream.write_all(msg.as_bytes())?;
        Ok(WatchStream {
            reader: BufReader::new(stream),
            pending: Vec::new(),
        })
    }

    /// Subscribe to changes in the *value* of `key` — what [`resolve`] makes
    /// of it, not one address's raw contents. See [`watch::ResolvedWatch`],
    /// which this opens.
    pub fn watch_resolved<'a>(
        &'a self,
        key: &str,
        inputs: &resolve::Inputs<'_>,
    ) -> Result<watch::ResolvedWatch<'a>, watch::WatchError> {
        watch::ResolvedWatch::open(self, key, inputs)
    }

    /// Ask the daemon for its protocol and build versions.
    pub fn hello(&self) -> Result<HelloInfo, CombError> {
        let socket_path = self.find_or_start_socket()?;
        let mut stream = self.connect(&socket_path)?;

        let request = serde_json::json!({ "op": "hello" });
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        stream.write_all(msg.as_bytes())?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let info = parse_hello_response(&line)?;
        if self.version_skew.get().is_none() {
            let _ = self.version_skew.set(detect_skew(&info));
        }
        Ok(info)
    }

    /// Open a persistent session for multiple queries on one connection.
    pub fn session(&self) -> Result<Session, CombError> {
        let socket_path = self.find_or_start_socket()?;
        let stream = self.connect(&socket_path)?;
        Ok(Session::new(stream))
    }

    fn find_or_start_socket(&self) -> Result<PathBuf, CombError> {
        if let Some(p) = &self.socket_path_override {
            return Ok(p.clone());
        }

        let path = socket_path();

        // Check if daemon is listening
        if UnixStream::connect(&path).is_ok() {
            return Ok(path);
        }

        if !self.config.auto_start {
            return Err(CombError::DaemonNotRunning);
        }

        // Try to start the daemon
        start_daemon(&path)?;

        // Wait for it to be ready
        let mut delay = Duration::from_millis(10);
        for _ in 0..8 {
            std::thread::sleep(delay);
            if UnixStream::connect(&path).is_ok() {
                return Ok(path);
            }
            delay = (delay * 2).min(Duration::from_millis(500));
        }

        Err(CombError::DaemonNotRunning)
    }

    fn connect(&self, path: &Path) -> Result<UnixStream, CombError> {
        let stream = connect_with_retry_for(path, self.connect_timeout)
            .map_err(CombError::ConnectionFailed)?;
        stream.set_read_timeout(Some(self.config.timeout))?;
        stream.set_write_timeout(Some(self.config.timeout))?;
        Ok(stream)
    }

    fn send_recv(
        &self,
        stream: &mut UnixStream,
        request: &serde_json::Value,
    ) -> Result<CombResult, CombError> {
        let line = self.send_recv_line(stream, request)?;
        parse_response(&line, self.skew_ref())
    }

    /// Send one request and read its one response line, probing for version
    /// skew afterwards. The parse is the caller's, so a `get` can be read as
    /// a [`CombResult`] or as a [`resolve::Got`].
    fn send_recv_line(
        &self,
        stream: &mut UnixStream,
        request: &serde_json::Value,
    ) -> Result<String, CombError> {
        let msg = format!("{}\n", serde_json::to_string(request).unwrap());
        stream.write_all(msg.as_bytes())?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut
            {
                CombError::Timeout
            } else {
                CombError::IoError(e)
            }
        })?;
        self.probe_skew_after(reader.get_mut());
        Ok(line)
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

/// [`resolve::Fetch`] over a [`Client`]: one daemon `get` per node, each on
/// its own connection.
///
/// A node with nothing in it — no `data`, no `template` — is a miss. A key
/// the daemon rejected (`ok: false`: an invalid facet)
/// is [`FetchError::Rejected`](resolve::FetchError::Rejected) in the daemon's
/// own words. Every other failure is the transport's: it is reported as
/// [`FetchError::Transport`](resolve::FetchError::Transport) and the
/// [`CombError`] behind it is kept in [`transport`](Self::transport), so a
/// surface that classifies failures by type (the C ABI's `error.kind`) reads
/// the error rather than parsing its message back.
pub struct ClientFetch<'a> {
    client: &'a Client,
    /// The failure behind the last `Transport` this fetch returned.
    pub transport: Option<CombError>,
}

impl<'a> ClientFetch<'a> {
    /// A fetch over `client`, with no failure recorded yet.
    pub fn new(client: &'a Client) -> Self {
        Self {
            client,
            transport: None,
        }
    }
}

impl resolve::Fetch for ClientFetch<'_> {
    fn get(
        &mut self,
        key: &str,
        path: Option<&str>,
        force: bool,
        wait: bool,
    ) -> Result<Option<resolve::Got>, resolve::FetchError> {
        match self.client.get_node(key, path, force, wait) {
            Ok(got) => Ok((got != resolve::Got::default()).then_some(got)),
            Err(CombError::ServerError(msg)) => Err(resolve::FetchError::Rejected(msg)),
            Err(e) => {
                let message = e.to_string();
                self.transport = Some(e);
                Err(resolve::FetchError::Transport(message))
            }
        }
    }
}

/// A persistent connection for multiple queries.
///
/// More efficient than individual `Client::get` calls when querying
/// multiple values in sequence (one connection vs. N connections).
pub struct Session {
    reader: BufReader<UnixStream>,
    /// Whether a version-skew probe has run yet on this connection.
    skew_checked: bool,
    /// Version skew, if any, discovered via a trailing `hello` exchange
    /// after this session's first op. Checked once per connection, not
    /// per op.
    version_skew: Option<VersionSkew>,
}

impl Session {
    fn new(stream: UnixStream) -> Self {
        Self {
            reader: BufReader::new(stream),
            skew_checked: false,
            version_skew: None,
        }
    }

    /// Version skew detected against the daemon on this session's
    /// connection, if any. `None` before the first op, or once probed, if
    /// the versions matched.
    pub fn version_skew(&self) -> Option<&VersionSkew> {
        self.version_skew.as_ref()
    }

    /// Probe for version skew after the caller's own request/response, if
    /// not already known this connection. Never sent before the caller's
    /// own request — see `Client::probe_skew_after` for why.
    fn probe_skew_after(&mut self) {
        if self.skew_checked {
            return;
        }
        self.skew_checked = true;
        self.version_skew = probe_version_skew(self.reader.get_mut()).unwrap_or(None);
    }

    /// Query a single key on this persistent connection. Raw, like
    /// [`Client::get`]: a template-only node is [`CombResult::Template`].
    pub fn get(&mut self, key: &str, path: Option<&str>) -> Result<CombResult, CombError> {
        self.get_with_flags(key, path, false, false)
    }

    /// Query a key with optional flags on this persistent connection.
    ///
    /// `force = true`: evict the cache entry and re-execute the provider before returning.
    /// `wait = true`: reserved for T14; currently a no-op.
    pub fn get_with_flags(
        &mut self,
        key: &str,
        path: Option<&str>,
        force: bool,
        wait: bool,
    ) -> Result<CombResult, CombError> {
        let line = self.send_recv_line(&get_request(key, path, force, wait))?;
        parse_response(&line, self.version_skew.as_ref())
    }

    /// Query one node on this persistent connection and return both halves
    /// of the daemon's answer — see [`Client::get_node`].
    pub fn get_node(
        &mut self,
        key: &str,
        path: Option<&str>,
        force: bool,
        wait: bool,
    ) -> Result<resolve::Got, CombError> {
        let line = self.send_recv_line(&get_request(key, path, force, wait))?;
        parse_node(&line, self.version_skew.as_ref())
    }

    /// Send one request on this connection and read its one response line,
    /// probing for version skew afterwards — the [`Client::send_recv_line`]
    /// of a session.
    fn send_recv_line(&mut self, request: &serde_json::Value) -> Result<String, CombError> {
        let msg = format!("{}\n", serde_json::to_string(request).unwrap());
        self.reader.get_mut().write_all(msg.as_bytes())?;

        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        self.probe_skew_after();
        Ok(line)
    }

    /// Query a single key on this persistent connection, rendered by the
    /// daemon as plain text. Shorthand for `get_formatted(key, path, "text")`.
    pub fn get_text(&mut self, key: &str, path: Option<&str>) -> Result<String, CombError> {
        self.get_formatted(key, path, "text")
    }

    /// Query a single key on this persistent connection, rendered
    /// server-side into `format` (e.g. `"text"`, `"sh"`).
    pub fn get_formatted(
        &mut self,
        key: &str,
        path: Option<&str>,
        format: &str,
    ) -> Result<String, CombError> {
        self.get_formatted_with_flags(key, path, format, false, false)
    }

    /// Query a key with optional flags on this persistent connection,
    /// rendered into `format` (`"text"` or `"sh"`) on the client side.
    ///
    /// `sh` shell-quotes every rendered value; `text` leaves them raw (see
    /// [`render::render_data`]). This performs a plain JSON `get` and renders
    /// the result locally (Task 1.8 retired the server-rendered wire
    /// sub-protocol this used to speak); a failed request surfaces as
    /// `CombError::ServerError`, never as a rendered `error:` line. This is
    /// raw like [`get`](Self::get): a template-only node renders its template
    /// text, not the value [`resolve`] would make of it.
    pub fn get_formatted_with_flags(
        &mut self,
        key: &str,
        path: Option<&str>,
        format: &str,
        force: bool,
        wait: bool,
    ) -> Result<String, CombError> {
        match self.get_with_flags(key, path, force, wait)? {
            CombResult::Hit { data, .. } => Ok(render::render_data(Some(data.as_value()), format)),
            CombResult::Template { template, .. } => {
                Ok(render::render_data(Some(&template), format))
            }
            CombResult::Miss => Ok(render::render_data(None, format)),
        }
    }

    /// Set connection context so subsequent queries don't need explicit paths.
    pub fn set_context(&mut self, path: &str) -> Result<(), CombError> {
        let request = serde_json::json!({ "op": "context", "path": path });
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        self.reader.get_mut().write_all(msg.as_bytes())?;

        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        self.probe_skew_after();
        check_ok(&line, self.version_skew.as_ref())?;
        Ok(())
    }

    /// Store data at a key, replacing whatever is stored at that address.
    pub fn put(
        &mut self,
        key: &str,
        data: serde_json::Value,
        ttl: Option<&str>,
        path: Option<&str>,
    ) -> Result<(), CombError> {
        let mut request = serde_json::json!({
            "op": "put",
            "key": key,
            "data": data,
        });
        if let Some(t) = ttl {
            request["ttl"] = serde_json::json!(t);
        }
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        self.reader.get_mut().write_all(msg.as_bytes())?;

        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        self.probe_skew_after();
        check_ok(&line, self.version_skew.as_ref())?;
        Ok(())
    }

    /// Trigger recomputation.
    pub fn refresh(&mut self, key: &str, path: Option<&str>) -> Result<(), CombError> {
        let mut request = serde_json::json!({ "op": "refresh", "key": key });
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        self.reader.get_mut().write_all(msg.as_bytes())?;

        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        self.probe_skew_after();
        check_ok(&line, self.version_skew.as_ref())?;
        Ok(())
    }

    /// List all cache entries currently held by the daemon.
    pub fn status(&mut self) -> Result<Vec<CacheRow>, CombError> {
        let request = serde_json::json!({ "op": "status" });
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        self.reader.get_mut().write_all(msg.as_bytes())?;
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        self.probe_skew_after();
        parse_cache_rows(&line, self.version_skew.as_ref())
    }

    /// [`Client::layers`] on this session's connection.
    pub fn layers(
        &mut self,
        key: &str,
        path: Option<&str>,
    ) -> Result<Option<serde_json::Value>, CombError> {
        let line = self.send_recv_line(&layers_request(key, path))?;
        parse_layers(&line, self.version_skew.as_ref())
    }

    pub fn introspect(
        &mut self,
        subject: IntrospectSubject,
        duration_secs: Option<u64>,
    ) -> Result<IntrospectResponse, CombError> {
        let mut request = serde_json::json!({
            "op": "introspect",
            "subject": subject.wire_name(),
        });
        if let Some(d) = duration_secs {
            request["duration_secs"] = serde_json::json!(d);
        }
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        self.reader.get_mut().write_all(msg.as_bytes())?;
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        self.probe_skew_after();
        parse_introspect(subject, &line, self.version_skew.as_ref())
    }

    /// Ask the daemon for its protocol and build versions.
    pub fn hello(&mut self) -> Result<HelloInfo, CombError> {
        let request = serde_json::json!({ "op": "hello" });
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        self.reader.get_mut().write_all(msg.as_bytes())?;

        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        let info = parse_hello_response(&line)?;
        if !self.skew_checked {
            self.skew_checked = true;
            self.version_skew = detect_skew(&info);
        }
        Ok(info)
    }
}

// --- Internal helpers ---

/// The `get` request every query sends: `path`, `force` and `wait` are
/// present only when set, so the wire form is the same whichever of
/// `get_with_flags` or `get_node` builds it, on either transport.
fn get_request(key: &str, path: Option<&str>, force: bool, wait: bool) -> serde_json::Value {
    let mut request = serde_json::json!({ "op": "get", "key": key });
    if let Some(p) = path {
        request["path"] = serde_json::json!(p);
    }
    if force {
        request["force"] = serde_json::json!(true);
    }
    if wait {
        request["wait"] = serde_json::json!(true);
    }
    request
}

/// Parse a response line's `{"ok": ...}` envelope, returning the raw JSON
/// value on success. On `ok:false`, returns `ServerError` with the
/// daemon's message — appended with `skew`, if known, so a failure on a
/// connection with detected version skew names both versions.
fn check_ok(line: &str, skew: Option<&VersionSkew>) -> Result<serde_json::Value, CombError> {
    let resp: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|e| CombError::ParseError(e.to_string()))?;
    let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if !ok {
        let mut error = resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string();
        if let Some(s) = skew {
            error = format!("{error} ({s})");
        }
        return Err(CombError::ServerError(error));
    }
    Ok(resp)
}

/// Parse a `get` response into a classified [`CombResult`]: [`parse_node`]
/// plus [`From<resolve::Got>`](CombResult::from) — one wire parser.
fn parse_response(line: &str, skew: Option<&VersionSkew>) -> Result<CombResult, CombError> {
    parse_node(line, skew).map(CombResult::from)
}

/// Parse a `get` response into both halves the daemon returned. Every field
/// is copied when present and left `None` when absent, so a miss is
/// `Got::default()`; `ok:false` is `ServerError` via [`check_ok`].
fn parse_node(line: &str, skew: Option<&VersionSkew>) -> Result<resolve::Got, CombError> {
    let resp = check_ok(line, skew)?;
    let present = |name: &str| resp.get(name).filter(|v| !v.is_null()).cloned();
    Ok(resolve::Got {
        data: present("data"),
        template: present("template"),
        rest: resp
            .get("rest")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        age_ms: resp
            .get("age_ms")
            .and_then(|v| v.as_u64())
            .map(|v| v as u128),
        stale: resp.get("stale").and_then(|v| v.as_bool()),
    })
}

fn layers_request(key: &str, path: Option<&str>) -> serde_json::Value {
    let mut request = serde_json::json!({ "op": "layers", "key": key });
    if let Some(p) = path {
        request["path"] = serde_json::json!(p);
    }
    request
}

fn parse_layers(
    line: &str,
    skew: Option<&VersionSkew>,
) -> Result<Option<serde_json::Value>, CombError> {
    let resp = check_ok(line, skew)?;
    Ok(resp.get("data").filter(|v| !v.is_null()).cloned())
}

fn parse_cache_rows(line: &str, skew: Option<&VersionSkew>) -> Result<Vec<CacheRow>, CombError> {
    let resp = check_ok(line, skew)?;
    let arr = resp
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| CombError::ParseError("status response data is not an array".into()))?;
    arr.iter().map(CacheRow::from_json).collect()
}

fn parse_hello_response(line: &str) -> Result<HelloInfo, CombError> {
    let resp: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|e| CombError::ParseError(e.to_string()))?;
    let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if !ok {
        let error = resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string();
        return Err(CombError::ServerError(error));
    }
    let data = resp
        .get("data")
        .ok_or_else(|| CombError::ParseError("hello response missing data field".into()))?;
    let protocol_version = data
        .get("protocol_version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CombError::ParseError("hello response missing protocol_version".into()))?
        .to_string();
    let daemon_version = data
        .get("daemon_version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CombError::ParseError("hello response missing daemon_version".into()))?
        .to_string();
    Ok(HelloInfo {
        protocol_version,
        daemon_version,
    })
}

fn parse_daemon_health(data: &serde_json::Value) -> Result<DaemonHealth, CombError> {
    let pid = data
        .get("pid")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| CombError::ParseError("daemon health missing pid".into()))?;
    let version = data
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CombError::ParseError("daemon health missing version".into()))?
        .to_string();
    let uptime_secs = data
        .get("uptime_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let socket_path = data
        .get("socket_path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let config_path = data
        .get("config_path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let requests_total = data
        .get("requests_total")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let in_flight = data.get("in_flight").and_then(|v| v.as_u64()).unwrap_or(0);
    let active_watchers = data
        .get("active_watchers")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_entries = data
        .get("cache_entries")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let watch_backend = data
        .get("watch_backend")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let reaper = data
        .get("reaper")
        .filter(|r| !r.is_null())
        .map(|r| ReaperStatus {
            armed: r.get("armed").and_then(|v| v.as_bool()).unwrap_or(false),
            visibility: r
                .get("visibility")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            sweeps: r.get("sweeps").and_then(|v| v.as_u64()).unwrap_or(0),
            reaped: r.get("reaped").and_then(|v| v.as_u64()).unwrap_or(0),
            kill_denied: r.get("kill_denied").and_then(|v| v.as_u64()).unwrap_or(0),
        });
    let verdicts = data
        .get("verdicts")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    let level = v.get("level")?.as_str()?.to_string();
                    let message = v.get("message")?.as_str()?.to_string();
                    Some(Verdict { level, message })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(DaemonHealth {
        pid,
        version,
        uptime_secs,
        socket_path,
        config_path,
        requests_total,
        in_flight,
        active_watchers,
        cache_entries,
        watch_backend,
        reaper,
        verdicts,
    })
}

fn parse_introspect(
    subject: IntrospectSubject,
    line: &str,
    skew: Option<&VersionSkew>,
) -> Result<IntrospectResponse, CombError> {
    let resp = check_ok(line, skew)?;
    let data = resp.get("data").cloned().unwrap_or(serde_json::Value::Null);
    match subject {
        IntrospectSubject::Daemon => Ok(IntrospectResponse::Daemon(parse_daemon_health(&data)?)),
        _ => Ok(IntrospectResponse::Other(data)),
    }
}

/// Find the beachcomber socket path.
///
/// Mirrors the daemon's bind-path resolution (`Config::resolve_socket_path`),
/// minus the config-file step which is daemon-only:
///   1. `$BEACHCOMBER_SOCKET` (if set and non-empty)
///   2. `/tmp/beachcomber-<uid>/sock`
///
/// There is no existence probe, and no session-scoped environment is
/// consulted (`$TMPDIR`, `$XDG_RUNTIME_DIR`): the result is the single stable
/// per-user path the daemon binds. Non-standard setups point clients at the
/// daemon via `BEACHCOMBER_SOCKET`.
pub fn socket_path() -> PathBuf {
    if let Some(env_path) = std::env::var_os("BEACHCOMBER_SOCKET")
        && !env_path.is_empty()
    {
        return PathBuf::from(env_path);
    }

    let uid = unsafe { libc::getuid() };
    PathBuf::from("/tmp")
        .join(format!("beachcomber-{}", uid))
        .join("sock")
}

/// Maximum usable unix socket path length in bytes, exclusive (`sun_path` is
/// 104 bytes on macOS, 108 on Linux; conservative bound used on both).
const MAX_SOCKET_PATH_BYTES: usize = 104;

/// Attempt to start the comb daemon via socket activation.
fn start_daemon(socket_path: &Path) -> Result<(), CombError> {
    use std::process::Command;

    // Pre-flight: a path the kernel cannot bind (SUN_LEN) would fork a daemon
    // doomed to fail; surface the real cause instead of a spawn timeout.
    if socket_path.as_os_str().len() >= MAX_SOCKET_PATH_BYTES {
        return Err(CombError::ConnectionFailed(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "socket path is {} bytes; unix sockets are limited to {} (SUN_LEN)",
                socket_path.as_os_str().len(),
                MAX_SOCKET_PATH_BYTES
            ),
        )));
    }

    // Find comb binary
    let comb = which_comb().ok_or(CombError::DaemonNotRunning)?;

    if let Some(parent) = socket_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut cmd = Command::new(&comb);
    cmd.arg("daemon")
        .arg("--socket")
        .arg(socket_path.as_os_str());
    // A spawn whose path came from $BEACHCOMBER_SOCKET is a deliberate
    // override daemon: flag it --no-reap so the reaping daemon spares it
    // (canon singleton.md §"Env-override spawns are flagged").
    if socket_path_is_env_override(socket_path) {
        cmd.arg("--no-reap");
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(CombError::ConnectionFailed)?;

    Ok(())
}

/// True when `socket_path` matches the value of `$BEACHCOMBER_SOCKET` — i.e.
/// the path being spawned at was supplied by the env override rather than the
/// per-user default.
fn socket_path_is_env_override(socket_path: &Path) -> bool {
    std::env::var_os("BEACHCOMBER_SOCKET")
        .filter(|v| !v.is_empty())
        .is_some_and(|v| Path::new(&v) == socket_path)
}

fn which_comb() -> Option<PathBuf> {
    // Check PATH for comb binary
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            let candidate = PathBuf::from(dir).join("comb");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `parse_response` classifies the eight response shapes of the one-tree
    /// design §3.4: `data` present is a `Hit` carrying whatever template
    /// half came with it; a template alone is a `Template`, with `rest` when
    /// the walk stopped at a string; neither is a `Miss`; `ok:false` is a
    /// server error. And it is `parse_node` plus `From<Got>` — the two
    /// parsers cannot disagree.
    #[test]
    fn parse_response_carries_the_template_half() {
        use serde_json::json;

        let hit = |value: serde_json::Value,
                   age_ms: u128,
                   stale: bool,
                   template: Option<serde_json::Value>| CombResult::Hit {
            data: CombData::from_json(value),
            age_ms,
            stale,
            template,
            rest: None,
        };
        let shapes: [(&str, CombResult); 7] = [
            (
                r#"{"ok":true,"template":"{{ env.SCANLINE_FOO }}"}"#,
                CombResult::Template {
                    template: json!("{{ env.SCANLINE_FOO }}"),
                    rest: None,
                },
            ),
            (
                r#"{"ok":true,"template":{"foo":"{{ a }}","bar":"{{ b }}"}}"#,
                CombResult::Template {
                    template: json!({"foo": "{{ a }}", "bar": "{{ b }}"}),
                    rest: None,
                },
            ),
            (
                r#"{"ok":true,"data":{"lit":1},"age_ms":30,"stale":false,"template":{"theme":"t","widgets":{"foo":"f"}}}"#,
                hit(
                    json!({"lit": 1}),
                    30,
                    false,
                    Some(json!({"theme": "t", "widgets": {"foo": "f"}})),
                ),
            ),
            (
                r#"{"ok":true,"data":"default","age_ms":812,"stale":false,"template":"{{ env.TF_WORKSPACE or comb.terraform.workspace }}"}"#,
                hit(
                    json!("default"),
                    812,
                    false,
                    Some(json!("{{ env.TF_WORKSPACE or comb.terraform.workspace }}")),
                ),
            ),
            (
                r#"{"ok":true,"data":"feature/x","age_ms":812,"stale":true}"#,
                hit(json!("feature/x"), 812, true, None),
            ),
            (
                r#"{"ok":true,"template":"{{ {'cpu': 40, 'mem': 12} }}","rest":"cpu"}"#,
                CombResult::Template {
                    template: json!("{{ {'cpu': 40, 'mem': 12} }}"),
                    rest: Some("cpu".to_string()),
                },
            ),
            (r#"{"ok":true}"#, CombResult::Miss),
        ];
        for (line, expected) in shapes {
            let parsed = parse_response(line, None).expect("an ok response parses");
            assert_eq!(parsed, expected, "{line}");
            assert_eq!(
                CombResult::from(parse_node(line, None).unwrap()),
                parsed,
                "parse_response is parse_node plus From<Got>: {line}"
            );
        }
        match parse_response(r#"{"ok":false,"error":"invalid key"}"#, None) {
            Err(CombError::ServerError(m)) => assert_eq!(m, "invalid key"),
            other => panic!("a rejected key must be a server error, got {other:?}"),
        }

        // The accessors: a `Template` is neither a hit nor a miss, and both
        // halves are reachable from whichever variant carries them.
        let template_only =
            parse_response(r#"{"ok":true,"template":"{{ x }}","rest":"cpu"}"#, None).unwrap();
        assert!(!template_only.is_hit());
        assert!(!template_only.is_miss());
        assert_eq!(template_only.template(), Some(&json!("{{ x }}")));
        assert_eq!(template_only.rest(), Some("cpu"));
        let both = parse_response(
            r#"{"ok":true,"data":1,"age_ms":0,"stale":false,"template":"{{ 2 }}"}"#,
            None,
        )
        .unwrap();
        assert!(both.is_hit());
        assert_eq!(both.template(), Some(&json!("{{ 2 }}")));
        assert_eq!(both.rest(), None);
        let miss = parse_response(r#"{"ok":true}"#, None).unwrap();
        assert!(miss.is_miss());
        assert_eq!(miss.template(), None);
    }

    /// `parse_node` keeps both halves: the eight response shapes of the
    /// one-tree design §3.4 — a template leaf, a template group, data plus a
    /// tree, data plus a template, data alone, a template plus `rest`, a
    /// miss, and a rejected key.
    #[test]
    fn parse_node_reads_every_response_shape() {
        use resolve::Got;
        use serde_json::json;

        let node = |line: &str| parse_node(line, None).expect("an ok response parses");

        assert_eq!(
            node(r#"{"ok":true,"template":"{{ env.SCANLINE_FOO }}"}"#),
            Got {
                template: Some(json!("{{ env.SCANLINE_FOO }}")),
                ..Got::default()
            }
        );
        assert_eq!(
            node(r#"{"ok":true,"template":{"foo":"{{ a }}","bar":"{{ b }}"}}"#),
            Got {
                template: Some(json!({"foo": "{{ a }}", "bar": "{{ b }}"})),
                ..Got::default()
            }
        );
        assert_eq!(
            node(
                r#"{"ok":true,"data":{"lit":1},"age_ms":30,"stale":false,"template":{"theme":"t","widgets":{"foo":"f"}}}"#
            ),
            Got {
                data: Some(json!({"lit": 1})),
                template: Some(json!({"theme": "t", "widgets": {"foo": "f"}})),
                rest: None,
                age_ms: Some(30),
                stale: Some(false),
            }
        );
        assert_eq!(
            node(
                r#"{"ok":true,"data":"default","age_ms":812,"stale":false,"template":"{{ env.TF_WORKSPACE or comb.terraform.workspace }}"}"#
            ),
            Got {
                data: Some(json!("default")),
                template: Some(json!("{{ env.TF_WORKSPACE or comb.terraform.workspace }}")),
                rest: None,
                age_ms: Some(812),
                stale: Some(false),
            }
        );
        assert_eq!(
            node(r#"{"ok":true,"data":"feature/x","age_ms":812,"stale":true}"#),
            Got {
                data: Some(json!("feature/x")),
                template: None,
                rest: None,
                age_ms: Some(812),
                stale: Some(true),
            }
        );
        assert_eq!(
            node(r#"{"ok":true,"template":"{{ {'cpu': 40, 'mem': 12} }}","rest":"cpu"}"#),
            Got {
                template: Some(json!("{{ {'cpu': 40, 'mem': 12} }}")),
                rest: Some("cpu".to_string()),
                ..Got::default()
            }
        );
        assert_eq!(node(r#"{"ok":true}"#), Got::default());
        match parse_node(r#"{"ok":false,"error":"invalid key"}"#, None) {
            Err(CombError::ServerError(m)) => assert_eq!(m, "invalid key"),
            other => panic!("a rejected key must be a server error, got {other:?}"),
        }
    }
}
