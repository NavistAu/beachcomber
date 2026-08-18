use crate::protocol::Response;
use libbeachcomber::{CombError, CombResult, HelloInfo, IntrospectResponse, IntrospectSubject};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Read/write timeout applied to every socket this adapter opens.
///
/// `libbeachcomber::ClientConfig` defaults to 100ms, which is tuned for its
/// own connect-probe/auto-spawn use, not for a `get`/`put` round trip: a
/// `force`/`wait` request blocks on the provider's actual execution, and
/// while this crate's own daemon caps that at `provider_timeout_secs`
/// (default 10s, see `config.rs`), a provider can be configured to run
/// longer. The pre-adapter root client set no timeout at all -- sockets
/// blocked until the daemon answered or the connection died -- and `watch`
/// in particular must keep blocking indefinitely between daemon-pushed
/// events. `ClientConfig::timeout` is a plain `Duration`, not
/// `Option<Duration>`, so "no timeout" isn't directly expressible; 30 days
/// is long enough to be a no-op for every real workload here (including
/// `watch`'s idle gaps) while staying safely under the ~49.7-day
/// (`u32::MAX` ms) ceiling some platforms impose on socket timeouts --
/// unlike the 100ms default, which is invisible against fast test
/// fixtures but would spuriously fail against a real, slower provider.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(60 * 60 * 24 * 30);

/// Build a `libbeachcomber` client bound to `socket_path`.
///
/// Trap: a bare `libbeachcomber::Client::new()` runs its own daemon
/// auto-spawn (`find_or_start_socket` / `start_daemon` / `which_comb`),
/// which would race `crate::daemon::ensure_daemon` and could start a
/// different `comb` binary than the one already resolved onto
/// `socket_path`. `with_socket_path` makes `find_or_start_socket` return
/// the override unconditionally, before `auto_start` is ever consulted;
/// `auto_start: false` is set anyway, belt-and-suspenders, so this stays
/// safe even if that short-circuit ever moves.
fn build_client(socket_path: &Path) -> libbeachcomber::Client {
    libbeachcomber::Client::with_config(libbeachcomber::ClientConfig {
        timeout: SOCKET_TIMEOUT,
        auto_start: false,
    })
    .with_socket_path(socket_path.to_path_buf())
}

/// Convert a transport-level failure into the `io::Error` the pre-adapter
/// client would have produced. `CombError::ServerError` -- the daemon
/// answering with `ok: false` -- is handled by `to_response` before
/// reaching here; it never represents a real transport failure.
fn comb_error_to_io(e: CombError) -> std::io::Error {
    match e {
        CombError::ConnectionFailed(io_err) | CombError::IoError(io_err) => io_err,
        other => std::io::Error::other(other.to_string()),
    }
}

/// Map a `libbeachcomber` result into the `Response`-shaped result the
/// pre-adapter wire-passthrough client produced: a daemon-level rejection
/// (`ok: false`) stays an `Ok(Response::error(..))`, exactly as it was
/// when the client parsed the raw response JSON directly instead of going
/// through `libbeachcomber`'s typed `Result`. Only a genuine transport
/// failure becomes `Err`.
fn to_response<T>(
    r: Result<T, CombError>,
    ok: impl FnOnce(T) -> Response,
) -> std::io::Result<Response> {
    match r {
        Ok(v) => Ok(ok(v)),
        Err(CombError::ServerError(msg)) => Ok(Response::error(msg)),
        Err(e) => Err(comb_error_to_io(e)),
    }
}

fn combresult_to_response(r: Result<CombResult, CombError>) -> std::io::Result<Response> {
    to_response(r, |v| match v {
        CombResult::Hit {
            data,
            age_ms,
            stale,
        } => Response::ok(data.as_value().clone(), age_ms, stale),
        CombResult::Miss => Response::miss(),
    })
}

fn unit_to_response(r: Result<(), CombError>) -> std::io::Result<Response> {
    to_response(r, |_| Response::miss())
}

fn hello_to_response(r: Result<HelloInfo, CombError>) -> std::io::Result<Response> {
    to_response(r, |info| {
        Response::ok(
            serde_json::json!({
                "protocol_version": info.protocol_version,
                "daemon_version": info.daemon_version,
            }),
            0,
            false,
        )
    })
}

fn row_kind_from_lib(k: libbeachcomber::RowKind) -> crate::cache::RowKind {
    match k {
        libbeachcomber::RowKind::Lifecycle {
            decay,
            watches_files,
        } => crate::cache::RowKind::Lifecycle {
            decay,
            watches_files,
        },
        libbeachcomber::RowKind::Once => crate::cache::RowKind::Once,
        libbeachcomber::RowKind::Virtual => crate::cache::RowKind::Virtual,
        libbeachcomber::RowKind::Transient => crate::cache::RowKind::Transient,
    }
}

fn failure_from_lib(f: libbeachcomber::FailureSnapshot) -> crate::cache::FailureSnapshot {
    crate::cache::FailureSnapshot {
        consecutive_failures: f.consecutive_failures,
        suppressed_until_unix_ms: f.suppressed_until_unix_ms,
    }
}

/// Map `libbeachcomber`'s wire-mirroring `CacheRow` onto this crate's own
/// `CacheRow` (used throughout `cli::status_format`). `field`/`source` are
/// non-optional here because the daemon always sets them; `libbeachcomber`
/// carries them as `Option` since it has no such guarantee about its wire
/// input.
fn cache_row_from_lib(r: libbeachcomber::CacheRow) -> crate::cache::CacheRow {
    crate::cache::CacheRow {
        provider: r.provider,
        path: r.path,
        source: r.source.unwrap_or_default(),
        field: r.field.unwrap_or_default(),
        value: r.value,
        age_ms: r.age_ms as u128,
        stale: r.stale,
        kind: r.kind.map(row_kind_from_lib),
        poll_interval_secs: r.poll_interval_secs,
        keep_alive_polls: r.keep_alive_polls,
        fsevents_reinstate: r.fsevents_reinstate,
        polls_elapsed: r.polls_elapsed,
        failure: r.failure.map(failure_from_lib),
    }
}

/// Map the wire subject name (as used by `cli/commands/check.rs` and
/// `cli/commands/status.rs`) onto `libbeachcomber`'s typed `IntrospectSubject`.
fn subject_from_str(s: &str) -> Option<IntrospectSubject> {
    Some(match s {
        "daemon" => IntrospectSubject::Daemon,
        "providers" => IntrospectSubject::Providers,
        "config" => IntrospectSubject::Config,
        "cache" => IntrospectSubject::Cache,
        "lifecycle" => IntrospectSubject::Lifecycle,
        "watches" => IntrospectSubject::Watches,
        "timers" => IntrospectSubject::Timers,
        "demand" => IntrospectSubject::Demand,
        "procs" => IntrospectSubject::Procs,
        _ => return None,
    })
}

/// Rebuild the wire-format JSON payload for a `Daemon` introspect response
/// from its typed shape, so the existing raw-JSON-payload callers
/// (`cli/commands/check.rs`, `cli/commands/status.rs`) don't need to change
/// their field access.
fn daemon_health_to_json(h: &libbeachcomber::DaemonHealth) -> serde_json::Value {
    let reaper = h.reaper.as_ref().map(|r| {
        serde_json::json!({
            "armed": r.armed,
            "visibility": r.visibility,
            "sweeps": r.sweeps,
            "reaped": r.reaped,
            "kill_denied": r.kill_denied,
        })
    });
    let verdicts: Vec<serde_json::Value> = h
        .verdicts
        .iter()
        .map(|v| serde_json::json!({"level": v.level, "message": v.message}))
        .collect();
    serde_json::json!({
        "pid": h.pid,
        "version": h.version,
        "uptime_secs": h.uptime_secs,
        "socket_path": h.socket_path,
        "config_path": h.config_path,
        "requests_total": h.requests_total,
        "in_flight": h.in_flight,
        "active_watchers": h.active_watchers,
        "cache_entries": h.cache_entries,
        "watch_backend": h.watch_backend,
        "reaper": reaper,
        "verdicts": verdicts,
    })
}

fn introspect_to_response(r: Result<IntrospectResponse, CombError>) -> std::io::Result<Response> {
    to_response(r, |v| match v {
        IntrospectResponse::Daemon(h) => Response::ok(daemon_health_to_json(&h), 0, false),
        IntrospectResponse::Other(value) => Response::ok(value, 0, false),
    })
}

pub struct Client {
    socket_path: PathBuf,
}

/// A persistent connection to the beachcomber daemon.
/// Reuses a single Unix socket connection across multiple requests,
/// avoiding the per-request connect/disconnect overhead.
pub struct ClientSession {
    session: libbeachcomber::Session,
    socket_path: PathBuf,
    watch_stream: Option<libbeachcomber::WatchStream>,
}

impl ClientSession {
    pub fn connect(socket_path: &Path) -> std::io::Result<Self> {
        let session = build_client(socket_path)
            .session()
            .map_err(comb_error_to_io)?;
        Ok(Self {
            session,
            socket_path: socket_path.to_path_buf(),
            watch_stream: None,
        })
    }

    pub fn set_context(&mut self, path: &str) -> std::io::Result<Response> {
        unit_to_response(self.session.set_context(path))
    }

    /// Ask the daemon for its protocol and build versions. Clients should
    /// call this on connection establishment to verify compatibility.
    pub fn hello(&mut self) -> std::io::Result<Response> {
        hello_to_response(self.session.hello())
    }

    pub fn get(&mut self, key: &str, path: Option<&str>) -> std::io::Result<Response> {
        self.get_with_flags(key, path, false, false)
    }

    pub fn get_with_flags(
        &mut self,
        key: &str,
        path: Option<&str>,
        force: bool,
        wait: bool,
    ) -> std::io::Result<Response> {
        combresult_to_response(self.session.get_with_flags(key, path, force, wait))
    }

    pub fn put(
        &mut self,
        key: &str,
        data: serde_json::Value,
        ttl: Option<&str>,
        path: Option<&str>,
    ) -> std::io::Result<Response> {
        unit_to_response(self.session.put(key, data, ttl, path))
    }

    /// Clear the cached entry for a virtual provider key without dropping the registry entry.
    /// A subsequent `put` under the same key still works.
    ///
    /// `ttl` is accepted for signature compatibility but, like the
    /// pre-adapter client, is inert here: the daemon's null-put path
    /// (`Request::Put` with `data: None`, see `server.rs`) never reads
    /// `ttl`, so dropping it before the wire is behavior-preserving --
    /// it matches both the old client's actual effect and
    /// `libbeachcomber::Session::put_null`'s signature, which has no
    /// `ttl` parameter at all.
    pub fn put_null(
        &mut self,
        key: &str,
        _ttl: Option<&str>,
        path: Option<&str>,
    ) -> std::io::Result<Response> {
        unit_to_response(self.session.put_null(key, path))
    }

    pub fn get_text(&mut self, key: &str, path: Option<&str>) -> std::io::Result<String> {
        self.get_formatted(key, path, "text")
    }

    pub fn get_formatted(
        &mut self,
        key: &str,
        path: Option<&str>,
        format: &str,
    ) -> std::io::Result<String> {
        self.get_formatted_with_flags(key, path, format, false, false)
    }

    pub fn get_formatted_with_flags(
        &mut self,
        key: &str,
        path: Option<&str>,
        format: &str,
        force: bool,
        wait: bool,
    ) -> std::io::Result<String> {
        self.session
            .get_formatted_with_flags(key, path, format, force, wait)
            .map_err(comb_error_to_io)
    }

    /// Send a watch request. Call read_watch_line() in a loop to receive updates.
    ///
    /// `libbeachcomber::Session` deliberately has no `watch`: the daemon
    /// puts a watched connection into permanent streaming mode, so no
    /// other op can share it afterward, which is the opposite of what
    /// `Session` (multiple ops, one connection) is for. So this opens a
    /// second, independent connection via `libbeachcomber::Client::watch`
    /// and switches this adapter over to reading from that -- meaning a
    /// `ClientSession` that calls `watch` holds two open sockets (this
    /// session's original connection, now idle, plus the watch stream)
    /// for its remaining lifetime, where the pre-adapter client issued
    /// `watch` directly on the session's one connection. Every current
    /// caller (`cli/commands/watch.rs`) opens a session purely to watch
    /// and does nothing else with it first, so this is a spare idle
    /// socket, not a correctness gap -- but it couldn't be closed without
    /// adding watch-on-`Session` support to `libbeachcomber`, which is out
    /// of scope for this task.
    pub fn watch(
        &mut self,
        key: &str,
        path: Option<&str>,
        format: Option<&str>,
    ) -> std::io::Result<()> {
        let stream = build_client(&self.socket_path)
            .watch(key, path, format)
            .map_err(comb_error_to_io)?;
        self.watch_stream = Some(stream);
        Ok(())
    }

    /// Read the next watch update line. Returns None on EOF.
    pub fn read_watch_line(&mut self) -> std::io::Result<Option<String>> {
        match &mut self.watch_stream {
            Some(stream) => stream.next_line().map_err(comb_error_to_io),
            None => Err(std::io::Error::other("read_watch_line called before watch")),
        }
    }
}

impl Client {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    fn inner(&self) -> libbeachcomber::Client {
        build_client(&self.socket_path)
    }

    /// Open a persistent session that reuses a single connection for multiple requests.
    pub fn connect(&self) -> std::io::Result<ClientSession> {
        ClientSession::connect(&self.socket_path)
    }

    pub fn get(&self, key: &str, path: Option<&str>) -> std::io::Result<Response> {
        self.get_with_flags(key, path, false, false)
    }

    pub fn get_with_flags(
        &self,
        key: &str,
        path: Option<&str>,
        force: bool,
        wait: bool,
    ) -> std::io::Result<Response> {
        combresult_to_response(self.inner().get_with_flags(key, path, force, wait))
    }

    pub fn get_text(&self, key: &str, path: Option<&str>) -> std::io::Result<String> {
        self.get_formatted(key, path, "text")
    }

    pub fn get_formatted(
        &self,
        key: &str,
        path: Option<&str>,
        format: &str,
    ) -> std::io::Result<String> {
        self.get_formatted_with_flags(key, path, format, false, false)
    }

    pub fn get_formatted_with_flags(
        &self,
        key: &str,
        path: Option<&str>,
        format: &str,
        force: bool,
        wait: bool,
    ) -> std::io::Result<String> {
        self.inner()
            .get_formatted_with_flags(key, path, format, force, wait)
            .map_err(comb_error_to_io)
    }

    pub fn put(
        &self,
        key: &str,
        data: serde_json::Value,
        ttl: Option<&str>,
        path: Option<&str>,
    ) -> std::io::Result<Response> {
        unit_to_response(self.inner().put(key, data, ttl, path))
    }

    /// Clear the cached entry for a virtual provider key without dropping the registry entry.
    /// A subsequent `put` under the same key still works.
    ///
    /// See `ClientSession::put_null` -- `ttl` is inert on the null-put path
    /// and is dropped before reaching `libbeachcomber`, which has no `ttl`
    /// parameter on this method.
    pub fn put_null(
        &self,
        key: &str,
        _ttl: Option<&str>,
        path: Option<&str>,
    ) -> std::io::Result<Response> {
        unit_to_response(self.inner().put_null(key, path))
    }

    pub fn refresh(&self, key: &str, path: Option<&str>) -> std::io::Result<Response> {
        unit_to_response(self.inner().refresh(key, path))
    }

    /// Ask the daemon for its protocol and build versions. Opens a new
    /// one-shot connection. Use `ClientSession::hello` instead if you
    /// already hold a persistent session.
    pub fn hello(&self) -> std::io::Result<Response> {
        hello_to_response(self.inner().hello())
    }

    /// List all cache entries currently held by the daemon, mapped onto
    /// this crate's own `CacheRow` (used by `cli::status_format`).
    pub fn status(&self) -> std::io::Result<Vec<crate::cache::CacheRow>> {
        self.inner()
            .status()
            .map(|rows| rows.into_iter().map(cache_row_from_lib).collect())
            .map_err(comb_error_to_io)
    }

    /// Run an introspect query. `subject` is the wire subject name
    /// (`"daemon"`, `"providers"`, `"config"`, ...); `duration_secs` is
    /// only consulted by the `procs` subject.
    ///
    /// Returns a `Response` shaped like the pre-adapter wire response, so
    /// the existing raw-JSON-payload callers (`cli/commands/check.rs`,
    /// `cli/commands/status.rs`) keep their field access unchanged: the
    /// `daemon` subject is rebuilt from its typed shape (see
    /// `daemon_health_to_json`), every other subject's
    /// `IntrospectResponse::Other` payload passes through as-is.
    pub fn introspect(
        &self,
        subject: &str,
        duration_secs: Option<u64>,
    ) -> std::io::Result<Response> {
        let subject = subject_from_str(subject).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unknown introspect subject: {subject}"),
            )
        })?;
        introspect_to_response(self.inner().introspect(subject, duration_secs))
    }
}
