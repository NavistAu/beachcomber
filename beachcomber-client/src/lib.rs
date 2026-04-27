//! # beachcomber-client
//!
//! A lightweight, synchronous client for the beachcomber (`comb`) shell state daemon.
//!
//! ```rust,no_run
//! use beachcomber_client::{Client, CombResult};
//!
//! let client = Client::new();
//! match client.get("git.branch", Some("/path/to/repo")) {
//!     Ok(CombResult::Hit { data, age_ms, stale }) => {
//!         println!("branch: {}", data.get_str("git.branch").unwrap_or("?"));
//!     }
//!     Ok(CombResult::Miss) => println!("not cached yet"),
//!     Err(e) => println!("error: {}", e),
//! }
//! ```

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Connect to a Unix socket with 3 retries (250ms / 500ms / 1s exponential backoff).
///
/// Retries on `ConnectionRefused` and `NotFound` only — other errors surface
/// immediately. Intended to cover the brief restart window when the old daemon
/// has shut down and the new one hasn't bound yet.
pub fn connect_with_retry(path: &Path) -> std::io::Result<UnixStream> {
    const BACKOFFS: [Duration; 3] = [
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_millis(1000),
    ];

    let mut last_err: Option<std::io::Error> = None;
    for backoff in &BACKOFFS {
        match UnixStream::connect(path) {
            Ok(s) => return Ok(s),
            Err(e) => {
                let kind = e.kind();
                if !matches!(
                    kind,
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) {
                    return Err(e);
                }
                last_err = Some(e);
                std::thread::sleep(*backoff);
            }
        }
    }
    // Final attempt after all backoffs.
    UnixStream::connect(path).map_err(|e| last_err.unwrap_or(e))
}

/// Result of a cache query.
#[derive(Debug)]
pub enum CombResult {
    /// Cache hit — data is available.
    Hit {
        data: CombData,
        age_ms: u128,
        stale: bool,
    },
    /// Cache miss — provider hasn't computed this yet.
    /// The daemon will compute it in the background; retry shortly.
    Miss,
}

/// Parsed response data from a provider.
#[derive(Debug, Clone)]
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

/// Discriminator used by the status formatter to choose rendering strategy.
/// Mirrors `beachcomber::cache::RowKind` for the wire format.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RowKind {
    Lifecycle { decay: u8, watches_files: bool },
    Once,
    Virtual,
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
    pub keep_alive_polls: Option<u32>,
    /// Phase 2.7: whether FSEvents will reinstate watching after a miss.
    pub fsevents_reinstate: Option<bool>,
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
        let keep_alive_polls = v
            .get("keep_alive_polls")
            .and_then(|x| x.as_u64().map(|n| n as u32));
        let fsevents_reinstate = v.get("fsevents_reinstate").and_then(|x| x.as_bool());
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
            keep_alive_polls,
            fsevents_reinstate,
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
    pub verdicts: Vec<Verdict>,
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
}

impl WatchStream {
    /// Read the next watch event. Returns Ok(None) on connection close.
    pub fn next_event(&mut self) -> Result<Option<WatchEvent>, CombError> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
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
        Ok(Some(WatchEvent {
            data,
            age_ms,
            stale,
        }))
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
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_millis(100),
            auto_start: true,
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
}

impl Client {
    /// Create a client with default configuration (100ms timeout, auto-start enabled).
    pub fn new() -> Self {
        Self {
            config: ClientConfig::default(),
            socket_path_override: None,
        }
    }

    /// Create a client with custom configuration.
    pub fn with_config(config: ClientConfig) -> Self {
        Self {
            config,
            socket_path_override: None,
        }
    }

    /// Override the socket path (bypassing auto-discovery). Primarily
    /// useful for tests that spawn a daemon on a custom socket.
    pub fn with_socket_path(mut self, path: PathBuf) -> Self {
        self.socket_path_override = Some(path);
        self
    }

    /// Query a single key. Returns Hit with data, Miss, or an error.
    ///
    /// Examples:
    /// - `client.get("git.branch", Some("/path/to/repo"))` — single field
    /// - `client.get("git", Some("/path/to/repo"))` — all fields
    /// - `client.get("hostname.short", None)` — global provider
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

        self.send_recv(&mut stream, &request)
    }

    /// Clear the cached entry for a virtual provider key without dropping the registry entry.
    /// A subsequent `put` under the same key still works.
    pub fn put_null(&self, key: &str, path: Option<&str>) -> Result<(), CombError> {
        let socket_path = self.find_or_start_socket()?;
        let mut stream = self.connect(&socket_path)?;

        let mut request = serde_json::json!({ "op": "put", "key": key });
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }

        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        stream.write_all(msg.as_bytes())?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        Ok(())
    }

    /// Store data into a virtual provider. `data` must be a JSON object;
    /// its top-level keys become provider fields.
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

        // Read response but don't care about content
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
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
        parse_cache_rows(&line)
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
        parse_introspect(subject, &line)
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
        })
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
        parse_hello_response(&line)
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

    fn connect(&self, path: &PathBuf) -> Result<UnixStream, CombError> {
        let stream = connect_with_retry(path).map_err(CombError::ConnectionFailed)?;
        stream.set_read_timeout(Some(self.config.timeout))?;
        stream.set_write_timeout(Some(self.config.timeout))?;
        Ok(stream)
    }

    fn send_recv(
        &self,
        stream: &mut UnixStream,
        request: &serde_json::Value,
    ) -> Result<CombResult, CombError> {
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

        parse_response(&line)
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

/// A persistent connection for multiple queries.
///
/// More efficient than individual `Client::get` calls when querying
/// multiple values in sequence (one connection vs. N connections).
pub struct Session {
    reader: BufReader<UnixStream>,
}

impl Session {
    fn new(stream: UnixStream) -> Self {
        Self {
            reader: BufReader::new(stream),
        }
    }

    /// Query a single key on this persistent connection.
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

        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        self.reader.get_mut().write_all(msg.as_bytes())?;

        let mut line = String::new();
        self.reader.read_line(&mut line)?;

        parse_response(&line)
    }

    /// Set connection context so subsequent queries don't need explicit paths.
    pub fn set_context(&mut self, path: &str) -> Result<(), CombError> {
        let request = serde_json::json!({ "op": "context", "path": path });
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        self.reader.get_mut().write_all(msg.as_bytes())?;

        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        Ok(())
    }

    /// Clear the cached entry for a virtual provider key without dropping the registry entry.
    pub fn put_null(&mut self, key: &str, path: Option<&str>) -> Result<(), CombError> {
        let mut request = serde_json::json!({ "op": "put", "key": key });
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        self.reader.get_mut().write_all(msg.as_bytes())?;

        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        Ok(())
    }

    /// Store data into a virtual provider.
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
        Ok(())
    }

    /// List all cache entries currently held by the daemon.
    pub fn status(&mut self) -> Result<Vec<CacheRow>, CombError> {
        let request = serde_json::json!({ "op": "status" });
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        self.reader.get_mut().write_all(msg.as_bytes())?;
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        parse_cache_rows(&line)
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
        parse_introspect(subject, &line)
    }

    /// Ask the daemon for its protocol and build versions.
    pub fn hello(&mut self) -> Result<HelloInfo, CombError> {
        let request = serde_json::json!({ "op": "hello" });
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        self.reader.get_mut().write_all(msg.as_bytes())?;

        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        parse_hello_response(&line)
    }
}

// --- Internal helpers ---

fn parse_response(line: &str) -> Result<CombResult, CombError> {
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

    match resp.get("data") {
        Some(serde_json::Value::Null) | None => Ok(CombResult::Miss),
        Some(data) => {
            let age_ms = resp
                .get("age_ms")
                .and_then(|v| v.as_u64())
                .map(|v| v as u128)
                .unwrap_or(0);
            let stale = resp.get("stale").and_then(|v| v.as_bool()).unwrap_or(false);
            Ok(CombResult::Hit {
                data: CombData {
                    value: data.clone(),
                },
                age_ms,
                stale,
            })
        }
    }
}

fn parse_cache_rows(line: &str) -> Result<Vec<CacheRow>, CombError> {
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
        verdicts,
    })
}

fn parse_introspect(
    subject: IntrospectSubject,
    line: &str,
) -> Result<IntrospectResponse, CombError> {
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
    let data = resp.get("data").cloned().unwrap_or(serde_json::Value::Null);
    match subject {
        IntrospectSubject::Daemon => Ok(IntrospectResponse::Daemon(parse_daemon_health(&data)?)),
        _ => Ok(IntrospectResponse::Other(data)),
    }
}

/// Find the beachcomber socket path.
pub fn socket_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let path = PathBuf::from(runtime_dir).join("beachcomber").join("sock");
        if path.exists() {
            return path;
        }
    }

    let uid = unsafe { libc::getuid() };
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(tmpdir)
        .join(format!("beachcomber-{}", uid))
        .join("sock")
}

/// Attempt to start the comb daemon via socket activation.
fn start_daemon(socket_path: &Path) -> Result<(), CombError> {
    use std::process::Command;

    // Find comb binary
    let comb = which_comb().ok_or(CombError::DaemonNotRunning)?;

    if let Some(parent) = socket_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    Command::new(&comb)
        .arg("daemon")
        .arg("--socket")
        .arg(socket_path.as_os_str())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(CombError::ConnectionFailed)?;

    Ok(())
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
