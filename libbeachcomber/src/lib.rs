//! # libbeachcomber
//!
//! A lightweight, synchronous client for the beachcomber (`comb`) shell state daemon.
//!
//! ```rust,no_run
//! use libbeachcomber::Client;
//!
//! let client = Client::new();
//! // What `comb get git.branch` prints in that directory; `Ok(None)` is a miss.
//! match client.get("git.branch", Some("/path/to/repo")) {
//!     Ok(Some(branch)) => println!("branch: {branch}"),
//!     Ok(None) => println!("nothing there"),
//!     Err(e) => println!("error: {}", e),
//! }
//! ```

pub mod duration;
pub mod engine;
pub mod env;
pub mod eval;
pub mod fs;
// `exec()` spawn engine, backing the `exec()` jinja function registered in
// `engine::build_engine_env`.
mod exec;
pub mod key;
mod lazy;
pub mod library;
pub mod path;
pub mod protocol;
pub mod render;
pub mod resolve;
pub mod socket;
pub mod spawn;
pub mod term;
pub mod validity;
pub mod virtual_fields;
pub mod watch;

/// This crate's build identity, derived by `build.rs` from `CARGO_PKG_VERSION`
/// plus git sha/dirty state. Shares its derivation with the root crate's
/// `BEACHCOMBER_VERSION` (see `build-common/version.rs`) so the two always
/// report the same string.
pub const VERSION: &str = env!("BEACHCOMBER_VERSION");

pub use protocol::{
    CacheRow, DaemonHealth, FailureSnapshot, HelloInfo, IntrospectSubject, ReaperStatus, RowKind,
    Verdict, Wait,
};
use protocol::{Request, Response};

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Connect to a Unix socket, retrying for up to [`DEFAULT_CONNECT_TIMEOUT`]
/// on the schedule [`connect_with_retry_for`] describes.
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
/// stream; drop it to disconnect. The socket is non-blocking: a read is
/// [`read_ready`](Self::read_ready)'s, made after a `poll(2)` said there is
/// something to read, and frames are taken from what it buffered.
pub struct WatchStream {
    socket: UnixStream,
    /// Bytes read and not yet taken as frames. A frame that arrived split
    /// across two reads waits here for its newline — bytes, not a `String`,
    /// since a split can fall mid-UTF-8-codepoint and only a whole line is
    /// ever decoded — and several frames read at once are taken one by one.
    buffer: Vec<u8>,
    /// The daemon closed the connection, or a read failed: nothing more will
    /// be read. Frames already buffered are still taken first.
    closed: bool,
}

impl WatchStream {
    /// A stream over `socket`, made non-blocking. Nothing may touch the
    /// socket's options once the daemon can have answered and closed it
    /// (macOS refuses a socket option on a unix socket whose peer has gone),
    /// so this comes before the request is sent.
    fn new(socket: UnixStream) -> std::io::Result<Self> {
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket,
            buffer: Vec::new(),
            closed: false,
        })
    }

    /// Read the next watch event, blocking until one arrives. Returns
    /// `Ok(None)` once the daemon has closed the connection and every frame
    /// it sent before closing has been read.
    pub fn next_event(&mut self) -> Result<Option<WatchEvent>, CombError> {
        loop {
            if let Some(frame) = self.take_frame() {
                return Self::parse_line(&frame?).map(Some);
            }
            if self.closed {
                return Ok(None);
            }
            watch::wait_readable(&[self.socket.as_raw_fd()], None, None)?;
            self.read_ready();
        }
    }

    /// Parse one NDJSON watch-event line into a [`WatchEvent`]: what
    /// [`Self::next_event`] answers, and what the fan-in
    /// ([`crate::watch::ResolvedWatch`]) reads a frame it took with.
    pub fn parse_line(line: &str) -> Result<WatchEvent, CombError> {
        let resp = envelope(line, None)?;
        Ok(WatchEvent {
            data: resp.data.map(CombData::from_json),
            age_ms: resp.age_ms.unwrap_or(0) as u64,
            stale: resp.stale.unwrap_or(false),
        })
    }

    /// Send the one request the connection carries.
    fn send(&self, request: &Request) -> Result<(), CombError> {
        (&self.socket).write_all(&request_line(request))?;
        Ok(())
    }

    /// The next whole frame already read, if there is one. `poll` cannot see
    /// these, so a caller takes them before it waits. A frame that is not
    /// UTF-8 is an error of kind `InvalidData`.
    pub(crate) fn take_frame(&mut self) -> Option<std::io::Result<String>> {
        let end = self.buffer.iter().position(|&b| b == b'\n')?;
        let frame: Vec<u8> = self.buffer.drain(..=end).collect();
        Some(
            String::from_utf8(frame)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    }

    /// Read everything the socket holds now, without blocking. The daemon
    /// closing the connection, or any read failing, marks the stream closed;
    /// a partial frame left at the end is no frame at all.
    pub(crate) fn read_ready(&mut self) {
        let mut chunk = [0u8; 8192];
        while !self.closed {
            match (&self.socket).read(&mut chunk) {
                Ok(0) => self.closed = true,
                Ok(n) => self.buffer.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(_) => self.closed = true,
            }
        }
    }

    /// Whether nothing more will be read: the daemon has closed the
    /// connection, or a read failed.
    pub(crate) fn is_closed(&self) -> bool {
        self.closed
    }
}

impl std::os::fd::AsRawFd for WatchStream {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.socket.as_raw_fd()
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

/// Two failures are the same when they are the same kind and say the same
/// thing; an I/O error has no equality of its own.
impl PartialEq for CombError {
    fn eq(&self, other: &Self) -> bool {
        self.kind() == other.kind() && self.to_string() == other.to_string()
    }
}

impl Eq for CombError {}

impl CombError {
    /// The stable slug naming this failure ([`kind`]): what the C ABI's
    /// `error.kind` carries, and what an SDK's error classes are named for.
    pub fn kind(&self) -> &'static str {
        match self {
            CombError::DaemonNotRunning => kind::DAEMON_NOT_RUNNING,
            CombError::ConnectionFailed(_) => kind::CONNECTION_FAILED,
            CombError::IoError(_) => kind::IO_ERROR,
            CombError::ParseError(_) => kind::PARSE_ERROR,
            CombError::ServerError(_) => kind::SERVER_ERROR,
            CombError::Timeout => kind::TIMEOUT,
        }
    }
}

/// The stable failure slugs: [`CombError::kind`],
/// [`resolve::ResolveError::kind`] and [`watch::WatchError::kind`] answer with
/// these.
pub mod kind {
    pub const DAEMON_NOT_RUNNING: &str = "daemon_not_running";
    pub const CONNECTION_FAILED: &str = "connection_failed";
    pub const IO_ERROR: &str = "io_error";
    pub const PARSE_ERROR: &str = "parse_error";
    pub const SERVER_ERROR: &str = "server_error";
    pub const TIMEOUT: &str = "timeout";
}

/// An I/O failure on a request. A read or write that ran out the client's
/// timeout is [`CombError::Timeout`], whichever op it was and on whichever
/// connection; anything else is [`CombError::IoError`].
impl From<std::io::Error> for CombError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => CombError::Timeout,
            _ => CombError::IoError(e),
        }
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

    /// One request on a fresh connection, answered: the reply's envelope.
    /// The first call also probes for version skew, with a trailing `hello`
    /// *after* the caller's own exchange — never before it — so it is checked
    /// once per `Client`, not per op, and a probe failure leaves the skew
    /// unknown rather than failing the op.
    fn call(&self, request: &Request) -> Result<Response, CombError> {
        let mut conn = self.open()?;
        let line = conn.exchange(request)?;
        if self.version_skew.get().is_none() {
            let _ = self.version_skew.set(conn.skew());
        }
        envelope(&line, self.skew_ref())
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

    /// The value `comb get key` prints, read from `path` (`None` is the global
    /// entry) through [`resolve::resolve`], the one function every surface
    /// shares: a template the daemon holds is rendered here, against this
    /// process's environment, and `Ok(None)` is a miss. `path` is read as every
    /// request path is ([`path::normalise_here`]).
    ///
    /// The unrendered stored value is a facet address, read through this same
    /// `get`: `client.get("git:cache.branch", path)`, `"aws:template.profile"`,
    /// `"p:put.x"`, `"p:source.x"`. For a caller's own `env`, `force` or
    /// `wait`, call [`resolve::resolve`] with an [`resolve::Inputs`] and
    /// `&mut &client` as the [`resolve::Fetch`]. For text, hand the value to
    /// [`render::render_value`].
    ///
    /// - `client.get("git.branch", Some("/path/to/repo"))` — one field
    /// - `client.get("git", Some("/path/to/repo"))` — all fields, as an object
    /// - `client.get("hostname.short", None)` — global provider
    pub fn get(
        &self,
        key: &str,
        path: Option<&str>,
    ) -> Result<Option<serde_json::Value>, resolve::ResolveError> {
        get_here(key, path, self.allow_exec(), &mut { self })
    }

    /// One node's two halves, as the resolver's [`Fetch`](resolve::Fetch)
    /// needs them: the cached `data` and the `template` in effect there. A
    /// miss is `Got::default()`, a rejected key `CombError::ServerError`.
    fn fetch_node(
        &self,
        key: &str,
        path: Option<&str>,
        force: bool,
        wait: Wait,
    ) -> Result<resolve::Got, CombError> {
        self.call(&get_request(key, path, force, wait)?)
            .map(parse_node)
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
        self.call(&put_request(key, data, ttl, path)?).map(|_| ())
    }

    /// Trigger recomputation of a provider. Fire-and-forget.
    pub fn refresh(&self, key: &str, path: Option<&str>) -> Result<(), CombError> {
        self.call(&refresh_request(key, path)?).map(|_| ())
    }

    /// List all cache entries currently held by the daemon.
    pub fn status(&self) -> Result<Vec<CacheRow>, CombError> {
        parse_cache_rows(self.call(&Request::Status {})?)
    }

    /// Run an introspect query. `duration_secs` is only consulted by the
    /// `procs` subject; ignored by others.
    pub fn introspect(
        &self,
        subject: IntrospectSubject,
        duration_secs: Option<u64>,
    ) -> Result<IntrospectResponse, CombError> {
        parse_introspect(
            subject,
            self.call(&introspect_request(subject, duration_secs))?,
        )
    }

    /// Subscribe to changes on a key. Returns a stream that blocks on
    /// `next_event` until the daemon emits a change (or the connection
    /// closes). The first event is always the current value.
    ///
    /// Watch is NOT available on Session because the daemon puts the
    /// connection into streaming mode once a watch is issued — no other
    /// ops can share that connection afterward.
    pub fn watch(&self, key: &str, path: Option<&str>) -> Result<WatchStream, CombError> {
        let stream = WatchStream::new(self.open()?.reader.into_inner())?;
        stream.send(&watch_request(key, path)?)?;
        Ok(stream)
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
        let info = self.open()?.hello()?;
        if self.version_skew.get().is_none() {
            let _ = self.version_skew.set(detect_skew(&info));
        }
        Ok(info)
    }

    /// Open a persistent session for multiple queries on one connection.
    pub fn session(&self) -> Result<Session, CombError> {
        Ok(Session::new(self.open()?, self.allow_exec()))
    }

    fn find_or_start_socket(&self) -> Result<PathBuf, CombError> {
        if let Some(p) = &self.socket_path_override {
            return Ok(p.clone());
        }

        let (path, source) = socket::resolve(None, None);
        if spawn::is_serving(&path) {
            return Ok(path);
        }
        if !self.config.auto_start {
            return Err(CombError::DaemonNotRunning);
        }
        // The same spawn the CLI makes: the first `comb` on PATH, its stderr
        // in the daemon's log, waited for until it says it serves.
        let comb = spawn::find_comb().ok_or(CombError::DaemonNotRunning)?;
        let log = spawn::log_path();
        let how = spawn::Spawn {
            binary: &comb,
            no_reap: source.no_reap(),
            log: &log,
        };
        match spawn::ensure(&path, &how) {
            Ok(()) => Ok(path),
            // A daemon that exited first or never came up is not running;
            // what it said is in its log.
            Err(spawn::Error::Exited { .. } | spawn::Error::TimedOut(_)) => {
                Err(CombError::DaemonNotRunning)
            }
            Err(spawn::Error::Socket(why)) => Err(CombError::ConnectionFailed(
                std::io::Error::new(std::io::ErrorKind::InvalidInput, why),
            )),
            Err(spawn::Error::Io(e)) => Err(CombError::ConnectionFailed(e)),
        }
    }

    /// A connection to the daemon, with this client's timeout on every read
    /// and write.
    fn open(&self) -> Result<Conn, CombError> {
        let socket = self.find_or_start_socket()?;
        let stream = connect_with_retry_for(&socket, self.connect_timeout)
            .map_err(CombError::ConnectionFailed)?;
        stream.set_read_timeout(Some(self.config.timeout))?;
        stream.set_write_timeout(Some(self.config.timeout))?;
        Ok(Conn {
            reader: BufReader::new(stream),
            timeout: self.config.timeout,
        })
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

/// [`resolve::Fetch`] over a [`Client`]: one daemon `get` per node, each on
/// its own connection.
impl resolve::Fetch for &Client {
    fn get(
        &mut self,
        key: &str,
        path: Option<&str>,
        force: bool,
        wait: Wait,
    ) -> Result<Option<resolve::Got>, resolve::FetchError> {
        fetched(self.fetch_node(key, path, force, wait))
    }
}

/// [`resolve::Fetch`] over a [`Session`]: one daemon `get` per node on its
/// connection. The path is the resolver's — `None` is the global entry — and
/// never the session's own directory.
impl resolve::Fetch for Session {
    fn get(
        &mut self,
        key: &str,
        path: Option<&str>,
        force: bool,
        wait: Wait,
    ) -> Result<Option<resolve::Got>, resolve::FetchError> {
        fetched(
            get_request(key, path, force, wait)
                .and_then(|request| self.call(&request))
                .map(parse_node),
        )
    }
}

/// A node as [`resolve`] reads it. Nothing in it — no `data`, no `template` —
/// is a miss. A key the daemon rejected (`ok: false`: an invalid facet) is
/// [`Rejected`](resolve::FetchError::Rejected) in the daemon's own words; any
/// other failure is the transport's, the [`CombError`] kept as it was.
fn fetched(
    node: Result<resolve::Got, CombError>,
) -> Result<Option<resolve::Got>, resolve::FetchError> {
    match node {
        Ok(got) => Ok((got != resolve::Got::default()).then_some(got)),
        Err(CombError::ServerError(msg)) => Err(resolve::FetchError::Rejected(msg)),
        Err(e) => Err(resolve::FetchError::Transport(e)),
    }
}

/// One connection to the daemon: a request line out, its response line back.
/// Every op on a [`Client`] or a [`Session`] is one [`Conn::exchange`].
struct Conn {
    reader: BufReader<UnixStream>,
    /// The client's timeout, on every read and write unless a request's own
    /// wait lengthens its read ([`Request::beyond_timeout`]).
    timeout: Duration,
}

/// A request as it goes on the wire: one JSON line.
fn request_line(request: &Request) -> Vec<u8> {
    let mut line = serde_json::to_vec(request).expect("a request serialises");
    line.push(b'\n');
    line
}

impl Conn {
    /// Write one request line.
    fn send(&mut self, request: &Request) -> Result<(), CombError> {
        self.reader.get_mut().write_all(&request_line(request))?;
        Ok(())
    }

    /// Send one request and read its one response line. The read may take
    /// the client's timeout plus whatever the request waits for (a `get`'s
    /// wait; `forever` has no deadline), and the connection's reads go back
    /// to the timeout after it.
    fn exchange(&mut self, request: &Request) -> Result<String, CombError> {
        let read_timeout = request
            .beyond_timeout()
            .map(|beyond| self.timeout.saturating_add(beyond));
        let lengthened = read_timeout != Some(self.timeout);
        if lengthened {
            self.reader.get_ref().set_read_timeout(read_timeout)?;
        }
        let answered = self.send(request).and_then(|()| {
            let mut line = String::new();
            self.reader.read_line(&mut line)?;
            Ok(line)
        });
        if lengthened {
            // A connection that cannot take the option back has failed, and
            // its next exchange says so: this one's answer stands.
            let _ = self.reader.get_ref().set_read_timeout(Some(self.timeout));
        }
        answered
    }

    /// The daemon's `hello`.
    fn hello(&mut self) -> Result<HelloInfo, CombError> {
        parse_hello(envelope(&self.exchange(&Request::Hello {})?, None)?)
    }

    /// The version skew a `hello` on this connection shows, if any; unknown
    /// (`None`) when the probe itself fails.
    fn skew(&mut self) -> Option<VersionSkew> {
        self.hello().ok().and_then(|info| detect_skew(&info))
    }
}

/// A persistent connection for multiple queries.
///
/// More efficient than individual `Client::get` calls when querying
/// multiple values in sequence (one connection vs. N connections).
pub struct Session {
    conn: Conn,
    /// Whether a version-skew probe has run yet on this connection.
    skew_checked: bool,
    /// Version skew, if any, discovered via a trailing `hello` exchange
    /// after this session's first op. Checked once per connection, not
    /// per op.
    version_skew: Option<VersionSkew>,
    /// The directory [`Session::set_context`] set, sent with every request
    /// that names no path.
    context: Option<String>,
    /// Whether templates this session resolves may call `exec()`.
    allow_exec: bool,
}

impl Session {
    fn new(conn: Conn, allow_exec: bool) -> Self {
        Self {
            conn,
            allow_exec,
            skew_checked: false,
            version_skew: None,
            context: None,
        }
    }

    /// Version skew detected against the daemon on this session's
    /// connection, if any. `None` before the first op, or once probed, if
    /// the versions matched.
    pub fn version_skew(&self) -> Option<&VersionSkew> {
        self.version_skew.as_ref()
    }

    /// One request on this connection, answered: the reply's envelope. The
    /// first call probes for version skew after the caller's own exchange,
    /// as [`Client`]'s does, once per connection.
    fn call(&mut self, request: &Request) -> Result<Response, CombError> {
        let line = self.conn.exchange(request)?;
        if !self.skew_checked {
            self.skew_checked = true;
            self.version_skew = self.conn.skew();
        }
        envelope(&line, self.version_skew.as_ref())
    }

    /// [`Client::get`] on this persistent connection: the value `comb get key`
    /// prints, `Ok(None)` a miss. A `path` of `None` addresses the session's
    /// directory ([`set_context`](Self::set_context)), else the global entry.
    pub fn get(
        &mut self,
        key: &str,
        path: Option<&str>,
    ) -> Result<Option<serde_json::Value>, resolve::ResolveError> {
        let path = self.path_or_context(path).map(str::to_string);
        let allow_exec = self.allow_exec;
        get_here(key, path.as_deref(), allow_exec, self)
    }

    /// Set this session's directory: every later request that names no path
    /// — `get`, `put`, `refresh`, `layers` alike — addresses it. Read as
    /// every path is ([`path::normalise_here`]); kept by the session and sent
    /// with each request, never set on the daemon's side of the connection.
    pub fn set_context(&mut self, path: &str) -> Result<(), CombError> {
        self.context = wire_path(Some(path))?;
        Ok(())
    }

    /// The directory [`set_context`](Self::set_context) set, if any.
    pub fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    /// The path a request on this session carries: its own, else the
    /// session's directory, else none (the global entry).
    fn path_or_context<'a>(&'a self, path: Option<&'a str>) -> Option<&'a str> {
        path.or(self.context.as_deref())
    }

    /// Store data at a key, replacing whatever is stored at that address.
    pub fn put(
        &mut self,
        key: &str,
        data: serde_json::Value,
        ttl: Option<&str>,
        path: Option<&str>,
    ) -> Result<(), CombError> {
        let request = put_request(key, data, ttl, self.path_or_context(path))?;
        self.call(&request).map(|_| ())
    }

    /// Trigger recomputation.
    pub fn refresh(&mut self, key: &str, path: Option<&str>) -> Result<(), CombError> {
        let request = refresh_request(key, self.path_or_context(path))?;
        self.call(&request).map(|_| ())
    }

    /// List all cache entries currently held by the daemon.
    pub fn status(&mut self) -> Result<Vec<CacheRow>, CombError> {
        parse_cache_rows(self.call(&Request::Status {})?)
    }

    /// The layers a read of `key` from `path` consults, per leaf: a tree
    /// shaped like the value with an ordered array of
    /// `{label, facet?, path, value, age_ms, stale, answered?}` at each leaf
    /// (a leaf key answers with its array). `None` is a miss. Warms nothing.
    /// A diagnostic for `comb status --tree`; no SDK binds it.
    pub fn layers(
        &mut self,
        key: &str,
        path: Option<&str>,
    ) -> Result<Option<serde_json::Value>, CombError> {
        let request = layers_request(key, self.path_or_context(path))?;
        self.call(&request).map(parse_layers)
    }

    /// [`Client::introspect`] on this session's connection.
    pub fn introspect(
        &mut self,
        subject: IntrospectSubject,
        duration_secs: Option<u64>,
    ) -> Result<IntrospectResponse, CombError> {
        parse_introspect(
            subject,
            self.call(&introspect_request(subject, duration_secs))?,
        )
    }

    /// Ask the daemon for its protocol and build versions.
    pub fn hello(&mut self) -> Result<HelloInfo, CombError> {
        let info = self.conn.hello()?;
        if !self.skew_checked {
            self.skew_checked = true;
            self.version_skew = detect_skew(&info);
        }
        Ok(info)
    }
}

// --- Internal helpers ---

/// The `get` request every query sends, whichever [`resolve::Fetch`] builds
/// it, on either transport.
fn get_request(
    key: &str,
    path: Option<&str>,
    force: bool,
    wait: Wait,
) -> Result<Request, CombError> {
    Ok(Request::Get {
        key: key.to_string(),
        path: wire_path(path)?,
        force,
        wait,
    })
}

fn put_request(
    key: &str,
    data: serde_json::Value,
    ttl: Option<&str>,
    path: Option<&str>,
) -> Result<Request, CombError> {
    Ok(Request::Put {
        key: key.to_string(),
        data,
        ttl: ttl.map(str::to_string),
        path: wire_path(path)?,
    })
}

fn refresh_request(key: &str, path: Option<&str>) -> Result<Request, CombError> {
    Ok(Request::Refresh {
        key: key.to_string(),
        path: wire_path(path)?,
    })
}

fn watch_request(key: &str, path: Option<&str>) -> Result<Request, CombError> {
    Ok(Request::Watch {
        key: key.to_string(),
        path: wire_path(path)?,
    })
}

fn layers_request(key: &str, path: Option<&str>) -> Result<Request, CombError> {
    Ok(Request::Layers {
        key: key.to_string(),
        path: wire_path(path)?,
    })
}

fn introspect_request(subject: IntrospectSubject, duration_secs: Option<u64>) -> Request {
    Request::Introspect {
        subject,
        duration_secs,
    }
}

/// The value `comb get key` prints at `path` (`None` is the global entry),
/// read from this process: its environment and directory conventions.
fn get_here(
    key: &str,
    path: Option<&str>,
    allow_exec: bool,
    fetch: &mut dyn resolve::Fetch,
) -> Result<Option<serde_json::Value>, resolve::ResolveError> {
    let transport = |e| resolve::ResolveError::Fetch(resolve::FetchError::Transport(e));
    let dir = wire_path(path).map_err(transport)?;
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let inputs = resolve::Inputs {
        cwd: dir.as_deref().unwrap_or(""),
        env: &env,
        slot: dir
            .clone()
            .map_or(resolve::Slot::Global, resolve::Slot::Path),
        force: false,
        wait: Wait::Budget,
        allow_exec,
        now: std::time::SystemTime::now(),
    };
    resolve::resolve(key, &inputs, fetch)
}

/// The `path` a request carries: [`path::normalise_here`], so `~` and a
/// relative directory mean what they mean to this process, and the daemon
/// only ever receives an absolute path. `None` is the global entry.
fn wire_path(path: Option<&str>) -> Result<Option<String>, CombError> {
    path.map(|p| path::normalise_here(Some(p)).map_err(|e| CombError::ParseError(e.to_string())))
        .transpose()
}

/// The one reading of a response line, for every op and every watch frame:
/// the [`Response`] on success. On `ok:false`, `ServerError` with the
/// daemon's message — appended with `skew`, if known, so a failure on a
/// connection with detected version skew names both versions.
fn envelope(line: &str, skew: Option<&VersionSkew>) -> Result<Response, CombError> {
    let resp: Response =
        serde_json::from_str(line.trim()).map_err(|e| CombError::ParseError(e.to_string()))?;
    if !resp.ok {
        let mut error = resp.error.unwrap_or_else(|| "unknown error".to_string());
        if let Some(s) = skew {
            error = format!("{error} ({s})");
        }
        return Err(CombError::ServerError(error));
    }
    Ok(resp)
}

/// A `get` response's halves, each `None` when absent, so a miss is
/// `Got::default()`.
fn parse_node(resp: Response) -> resolve::Got {
    resolve::Got {
        data: resp.data,
        template: resp.template,
        rest: resp.rest,
        memo: resp.memo,
        age_ms: resp.age_ms,
        stale: resp.stale,
    }
}

fn parse_layers(resp: Response) -> Option<serde_json::Value> {
    resp.data
}

/// A response's `data` as the payload type its op answers with.
fn payload<T: serde::de::DeserializeOwned>(resp: Response, op: &str) -> Result<T, CombError> {
    let data = resp
        .data
        .ok_or_else(|| CombError::ParseError(format!("{op} response has no data")))?;
    serde_json::from_value(data).map_err(|e| CombError::ParseError(format!("{op} response: {e}")))
}

fn parse_cache_rows(resp: Response) -> Result<Vec<CacheRow>, CombError> {
    payload(resp, "status")
}

fn parse_hello(resp: Response) -> Result<HelloInfo, CombError> {
    payload(resp, "hello")
}

fn parse_introspect(
    subject: IntrospectSubject,
    resp: Response,
) -> Result<IntrospectResponse, CombError> {
    match subject {
        IntrospectSubject::Daemon => Ok(IntrospectResponse::Daemon(payload(resp, "introspect")?)),
        _ => Ok(IntrospectResponse::Other(
            resp.data.unwrap_or(serde_json::Value::Null),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `get` response line read as a client reads it.
    fn node_of(line: &str) -> Result<resolve::Got, CombError> {
        envelope(line, None).map(parse_node)
    }

    /// Every failure has its slug, and a read or write that ran out its
    /// timeout is a timeout, not an I/O error.
    #[test]
    fn every_failure_has_its_kind() {
        let io = |k| std::io::Error::new(k, "x");
        for (e, slug) in [
            (CombError::DaemonNotRunning, "daemon_not_running"),
            (
                CombError::ConnectionFailed(io(std::io::ErrorKind::ConnectionRefused)),
                "connection_failed",
            ),
            (
                CombError::IoError(io(std::io::ErrorKind::BrokenPipe)),
                "io_error",
            ),
            (CombError::ParseError("x".into()), "parse_error"),
            (CombError::ServerError("x".into()), "server_error"),
            (CombError::Timeout, "timeout"),
            (
                CombError::from(io(std::io::ErrorKind::WouldBlock)),
                "timeout",
            ),
            (CombError::from(io(std::io::ErrorKind::TimedOut)), "timeout"),
            (
                CombError::from(io(std::io::ErrorKind::BrokenPipe)),
                "io_error",
            ),
        ] {
            assert_eq!(e.kind(), slug, "{e:?}");
        }
    }

    /// A `get` response read as a `Got` keeps every half: the eight response shapes of the
    /// one-tree design §3.4 — a template leaf, a template group, data plus a
    /// tree, data plus a template, data alone, a template plus `rest`, a
    /// miss, and a rejected key — and a template with its memo half.
    #[test]
    fn parse_node_reads_every_response_shape() {
        use resolve::Got;
        use serde_json::json;

        let node = |line: &str| node_of(line).expect("an ok response parses");

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
                memo: None,
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
                memo: None,
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
                memo: None,
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
        assert_eq!(
            node(
                r#"{"ok":true,"template":{"a":"{{ x }}","b":"{{ y }}"},"memo":{"a":{"hash":"9c6001d328a94690","stale":false,"age_ms":4,"data":"prod"}}}"#
            ),
            Got {
                template: Some(json!({"a": "{{ x }}", "b": "{{ y }}"})),
                memo: Some(
                    json!({"a": {"hash": "9c6001d328a94690", "stale": false, "age_ms": 4, "data": "prod"}})
                ),
                ..Got::default()
            }
        );
        assert_eq!(node(r#"{"ok":true}"#), Got::default());
        match node_of(r#"{"ok":false,"error":"invalid key"}"#) {
            Err(CombError::ServerError(m)) => assert_eq!(m, "invalid key"),
            other => panic!("a rejected key must be a server error, got {other:?}"),
        }
    }

    /// A watch stream over one end of a pair; the other end is the daemon's.
    fn watch_pair() -> (WatchStream, UnixStream) {
        let (ours, daemon) = UnixStream::pair().expect("a pair");
        (WatchStream::new(ours).expect("a watch stream"), daemon)
    }

    /// Wait for `stream` to be readable, then read it, as the fan-in does.
    fn read_when_ready(stream: &mut WatchStream) {
        let deadline = Instant::now() + Duration::from_secs(5);
        watch::wait_readable(&[stream.as_raw_fd()], None, Some(deadline)).expect("wait");
        stream.read_ready();
    }

    fn frame(stream: &mut WatchStream) -> String {
        stream
            .take_frame()
            .expect("a whole frame")
            .expect("a UTF-8 frame")
    }

    /// Two frames in one write are two frames from one read: the second is
    /// already buffered, so it needs no second wait.
    #[test]
    fn two_frames_in_one_write_are_two_frames_from_one_read() {
        let (mut stream, daemon) = watch_pair();
        (&daemon)
            .write_all(b"{\"ok\":true,\"data\":1}\n{\"ok\":true,\"data\":2}\n")
            .expect("write");

        read_when_ready(&mut stream);
        assert_eq!(frame(&mut stream), "{\"ok\":true,\"data\":1}\n");
        assert_eq!(frame(&mut stream), "{\"ok\":true,\"data\":2}\n");
        assert!(stream.take_frame().is_none());
        assert!(!stream.is_closed());
    }

    /// A frame split mid-codepoint across two writes is no frame until its
    /// second half lands, and then it is whole.
    #[test]
    fn a_frame_split_mid_codepoint_resumes() {
        let (mut stream, daemon) = watch_pair();
        let whole = "{\"ok\":true,\"data\":\"é\"}\n".as_bytes();
        let split = whole.iter().position(|&b| b == 0xC3).expect("é") + 1;

        (&daemon).write_all(&whole[..split]).expect("write");
        read_when_ready(&mut stream);
        assert!(stream.take_frame().is_none(), "half a frame is no frame");

        (&daemon).write_all(&whole[split..]).expect("write");
        read_when_ready(&mut stream);
        let event = WatchStream::parse_line(&frame(&mut stream)).expect("an event");
        assert_eq!(
            event.data.expect("data").as_value(),
            &serde_json::json!("é")
        );
    }

    /// A frame the daemon sent before it closed the connection is read before
    /// the end of the stream: the refusal is the answer, not end-of-stream.
    #[test]
    fn a_frame_sent_before_the_daemon_closed_comes_before_the_end() {
        let (mut stream, daemon) = watch_pair();
        (&daemon)
            .write_all(b"{\"ok\":false,\"error\":\"invalid facet key: a:b:c\"}\n")
            .expect("write");
        drop(daemon);

        match stream.next_event() {
            Err(CombError::ServerError(m)) => assert!(m.contains("invalid facet key"), "{m}"),
            other => panic!("the refusal comes first, got {other:?}"),
        }
        assert!(matches!(stream.next_event(), Ok(None)), "then the end");
    }
}
