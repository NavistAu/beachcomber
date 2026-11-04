use crate::protocol::Response;
use libbeachcomber::{CombError, IntrospectResponse, IntrospectSubject};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Read/write timeout applied to every socket this adapter opens: how long a
/// command waits on a daemon that says nothing before it says so itself.
///
/// `libbeachcomber::ClientConfig` defaults to 100ms, which is tuned for its
/// own connect-probe/auto-spawn use, not for a `get`/`put` round trip: a cold
/// or forced read waits for the Source's run, which the daemon cuts off at the
/// Source's `timeout` (default 10s, `[daemon] timeout`). This outlasts a chain
/// of those. A Source configured to run longer than this needs a read that
/// says so (`--wait`, which lengthens its own read); `watch` does not read
/// under this timeout at all.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(60);

/// Build a `libbeachcomber` client bound to `socket_path`.
///
/// Trap: a bare `libbeachcomber::Client::new()` runs its own daemon
/// auto-spawn (`find_or_start_socket`, which starts the first `comb` on
/// `PATH`), which would race `crate::cli::connect::connect` and could start a
/// different `comb` binary than the one already resolved onto
/// `socket_path`. `with_socket_path` makes `find_or_start_socket` return
/// the override unconditionally, before `auto_start` is ever consulted;
/// `auto_start: false` is set anyway, belt-and-suspenders, so this stays
/// safe even if that short-circuit ever moves.
pub fn build_client(socket_path: &Path) -> libbeachcomber::Client {
    client_with_timeout(socket_path, SOCKET_TIMEOUT)
}

fn client_with_timeout(socket_path: &Path, timeout: Duration) -> libbeachcomber::Client {
    libbeachcomber::Client::with_config(libbeachcomber::ClientConfig {
        timeout,
        auto_start: false,
        // `exec` governs `bc_get`/`bc_eval` through `Client::allow_exec`,
        // which the CLI never calls: each command decides at its own `Inputs`
        // (`allow_exec: true` in `get` and `eval`). This flag is never read here.
        ..libbeachcomber::ClientConfig::default()
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

fn unit_to_response(r: Result<(), CombError>) -> std::io::Result<Response> {
    to_response(r, |_| Response::miss())
}

fn introspect_to_response(r: Result<IntrospectResponse, CombError>) -> std::io::Result<Response> {
    to_response(r, |v| match v {
        IntrospectResponse::Daemon(h) => Response::ok(
            serde_json::to_value(h).expect("daemon health serialises"),
            0,
            false,
        ),
        IntrospectResponse::Other(value) => Response::ok(value, 0, false),
    })
}

pub struct Client {
    socket_path: PathBuf,
    /// Total time a connect keeps retrying before giving up.
    connect_timeout: Duration,
}

impl Client {
    pub fn new(socket_path: PathBuf) -> Self {
        Self::with_connect_timeout(socket_path, libbeachcomber::DEFAULT_CONNECT_TIMEOUT)
    }

    /// A client whose connects give up after `connect_timeout` in total.
    /// Zero means a single attempt.
    pub fn with_connect_timeout(socket_path: PathBuf, connect_timeout: Duration) -> Self {
        Self {
            socket_path,
            connect_timeout,
        }
    }

    fn inner(&self) -> libbeachcomber::Client {
        build_client(&self.socket_path).with_connect_timeout(self.connect_timeout)
    }

    /// Open a persistent session that reuses a single connection for
    /// multiple requests. The session is also the CLI's transport for
    /// [`libbeachcomber::resolve`].
    pub fn connect(&self) -> Result<libbeachcomber::Session, CombError> {
        build_client(&self.socket_path).session()
    }

    /// Store `data` at `key`, replacing whatever is stored at that address.
    pub fn put(
        &self,
        key: &str,
        data: serde_json::Value,
        ttl: Option<&str>,
        path: Option<&str>,
    ) -> std::io::Result<Response> {
        unit_to_response(self.inner().put(key, data, ttl, path))
    }

    pub fn refresh(&self, key: &str, path: Option<&str>) -> std::io::Result<Response> {
        unit_to_response(self.inner().refresh(key, path))
    }

    /// List all cache entries currently held by the daemon.
    pub fn status(&self) -> std::io::Result<Vec<crate::cache::CacheRow>> {
        self.inner().status().map_err(comb_error_to_io)
    }

    /// Run an introspect query. `subject` is the wire subject name
    /// (`"daemon"`, `"providers"`, `"config"`, ...); `duration_secs` is
    /// only consulted by the `procs` subject.
    ///
    /// Returns a `Response` shaped like the wire response, so the
    /// raw-JSON-payload callers (`cli/commands/check.rs`,
    /// `cli/commands/status.rs`) keep their field access: the `daemon`
    /// subject is its typed payload serialised as the wire carries it, every
    /// other subject's payload passes through as-is.
    pub fn introspect(
        &self,
        subject: &str,
        duration_secs: Option<u64>,
    ) -> std::io::Result<Response> {
        let subject: IntrospectSubject = subject
            .parse()
            .map_err(|e: String| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        introspect_to_response(self.inner().introspect(subject, duration_secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A daemon that takes the connection and never answers ends in a timeout,
    /// on one-shot calls and sessions both: `comb get` sat on this read for as
    /// long as such a daemon lived.
    #[test]
    fn a_daemon_that_never_answers_is_a_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("sock");
        let _silent = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let client = client_with_timeout(&socket, Duration::from_millis(20));
        assert!(matches!(client.hello(), Err(CombError::Timeout)));
        let mut session = client.session().unwrap();
        assert!(matches!(session.hello(), Err(CombError::Timeout)));
    }
}
