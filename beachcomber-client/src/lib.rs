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
use std::path::PathBuf;
use std::time::Duration;

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
}

impl Client {
    /// Create a client with default configuration (100ms timeout, auto-start enabled).
    pub fn new() -> Self {
        Self {
            config: ClientConfig::default(),
        }
    }

    /// Create a client with custom configuration.
    pub fn with_config(config: ClientConfig) -> Self {
        Self { config }
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
        let stream = UnixStream::connect(path).map_err(CombError::ConnectionFailed)?;
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
fn start_daemon(socket_path: &PathBuf) -> Result<(), CombError> {
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
