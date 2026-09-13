//! C ABI surface for `libbeachcomber`. This crate contains no logic of its
//! own — every `extern "C"` entry point is a thin wrapper delegating to
//! `libbeachcomber`.

use std::ffi::{CStr, CString, c_char};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub mod envelope;

use envelope::{ErrorKind, FfiError, WatchOutcome, call_ffi, call_watch_next};
use libbeachcomber::resolve::{self, FetchError, Inputs, ResolveError, Slot};
use libbeachcomber::{
    CacheRow, ClientFetch, CombResult, DaemonHealth, IntrospectResponse, IntrospectSubject,
};
use std::collections::HashMap;

/// Opaque client handle returned by [`bc_client_new`].
///
/// Construction never fails from the caller's point of view: a malformed
/// `options_json` is recorded here and surfaced on the handle's first
/// operation instead of failing the constructor.
pub struct BcClient {
    state: Result<libbeachcomber::Client, FfiError>,
}

static VERSION_CSTRING: OnceLock<CString> = OnceLock::new();

/// Returns the library's build version as a static, NUL-terminated string
/// that must **not** be passed to [`envelope::bc_string_free`].
#[unsafe(no_mangle)]
pub extern "C" fn bc_version() -> *const c_char {
    VERSION_CSTRING
        .get_or_init(|| {
            CString::new(libbeachcomber::VERSION)
                .expect("BEACHCOMBER_VERSION must not contain a NUL byte")
        })
        .as_ptr()
}

/// Recognised keys of `bc_client_new`'s `options_json` argument.
#[derive(serde::Deserialize, Default)]
struct ClientOptions {
    socket_path: Option<String>,
    timeout_ms: Option<u64>,
    autostart: Option<bool>,
    exec: Option<bool>,
}

/// # Safety
/// `options_json` must be NULL or a valid pointer to a NUL-terminated string.
unsafe fn build_client(options_json: *const c_char) -> Result<libbeachcomber::Client, FfiError> {
    let options = if options_json.is_null() {
        ClientOptions::default()
    } else {
        let raw = unsafe { CStr::from_ptr(options_json) }.to_string_lossy();
        serde_json::from_str::<ClientOptions>(&raw).map_err(|e| {
            FfiError::new(
                ErrorKind::ParseError,
                format!("malformed options_json: {e}"),
            )
        })?
    };

    let default_config = libbeachcomber::ClientConfig::default();
    let config = libbeachcomber::ClientConfig {
        timeout: options
            .timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(default_config.timeout),
        auto_start: options.autostart.unwrap_or(default_config.auto_start),
        exec: options.exec.unwrap_or(default_config.exec),
    };

    let mut client = libbeachcomber::Client::with_config(config);
    if let Some(socket_path) = options.socket_path {
        client = client.with_socket_path(std::path::PathBuf::from(socket_path));
    }
    Ok(client)
}

/// Creates a new client handle. `options_json` is nullable; recognised keys
/// are `socket_path`, `timeout_ms`, `autostart` and `exec` — whether a
/// template this client resolves (`bc_resolve`, `bc_eval`) may call `exec()`,
/// default `false`. Never returns NULL — a malformed `options_json` still
/// yields a handle, with the parse error deferred to the handle's first
/// operation rather than failing here.
///
/// # Safety
/// `options_json` must be NULL or a valid pointer to a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_client_new(options_json: *const c_char) -> *mut BcClient {
    let state = unsafe { build_client(options_json) };
    Box::into_raw(Box::new(BcClient { state }))
}

/// Frees a client handle. Null-safe.
///
/// # Safety
/// `client` must be either NULL or a pointer previously returned by
/// [`bc_client_new`], not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_client_free(client: *mut BcClient) {
    if client.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(client) });
}

// ── Operations (Task 3.4) ──────────────────────────────────────────────

/// `bc_get` flag: evict the cache entry and re-execute the provider before
/// returning.
pub const BC_GET_FORCE: u32 = 1 << 0;
/// `bc_get` flag: if the entry is stale, wait for inline re-execution.
pub const BC_GET_WAIT: u32 = 1 << 1;
const BC_GET_KNOWN_FLAGS: u32 = BC_GET_FORCE | BC_GET_WAIT;

/// Rejects (rather than silently ignoring) any bit outside 0..=1.
fn check_get_flags(flags: u32) -> Result<(), FfiError> {
    let unknown = flags & !BC_GET_KNOWN_FLAGS;
    if unknown != 0 {
        return Err(FfiError::new(
            ErrorKind::BadFlags,
            format!("unknown flag bits set: {unknown:#x}"),
        ));
    }
    Ok(())
}

/// Borrows the handle's underlying client, or its deferred construction
/// error.
///
/// # Safety
/// `client` must be a valid, non-null pointer previously returned by
/// [`bc_client_new`], not yet freed.
unsafe fn client_ref<'a>(client: *mut BcClient) -> Result<&'a libbeachcomber::Client, FfiError> {
    let handle = unsafe { &*client };
    handle.state.as_ref().map_err(|e| e.clone())
}

/// # Safety
/// `ptr` must be NUL-terminated and valid for the duration of the call.
unsafe fn required_str<'a>(ptr: *const c_char, name: &str) -> Result<&'a str, FfiError> {
    if ptr.is_null() {
        return Err(FfiError::new(
            ErrorKind::ParseError,
            format!("{name} must not be NULL"),
        ));
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().map_err(|e| {
        FfiError::new(
            ErrorKind::ParseError,
            format!("{name} is not valid UTF-8: {e}"),
        )
    })
}

/// # Safety
/// `ptr` must be NULL or NUL-terminated and valid for the duration of the
/// call.
unsafe fn optional_str<'a>(ptr: *const c_char) -> Result<Option<&'a str>, FfiError> {
    if ptr.is_null() {
        return Ok(None);
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map(Some)
        .map_err(|e| {
            FfiError::new(
                ErrorKind::ParseError,
                format!("argument is not valid UTF-8: {e}"),
            )
        })
}

/// The `get` envelope's inner object. `data`/`age_ms`/`stale` are always
/// present (null on anything but a hit); `template` and `rest` appear only
/// when the node carries them — so a data-only hit and a miss are unchanged
/// from before the template half existed.
fn combresult_to_json(result: CombResult) -> serde_json::Value {
    let mut inner = match &result {
        CombResult::Hit {
            data,
            age_ms,
            stale,
            ..
        } => serde_json::json!({
            "data": data.as_value(),
            "age_ms": *age_ms as u64,
            "stale": stale,
        }),
        CombResult::Template { .. } | CombResult::Miss => serde_json::json!({
            "data": null,
            "age_ms": null,
            "stale": null,
        }),
    };
    if let Some(template) = result.template() {
        inner["template"] = template.clone();
    }
    if let Some(rest) = result.rest() {
        inner["rest"] = serde_json::Value::String(rest.to_string());
    }
    inner
}

fn cache_row_to_json(r: &CacheRow) -> serde_json::Value {
    serde_json::json!({
        "provider": r.provider,
        "field": r.field,
        "path": r.path,
        "value": r.value,
        "age_ms": r.age_ms,
        "stale": r.stale,
        "kind": r.kind,
        "poll_interval_secs": r.poll_interval_secs,
        "keep_alive_polls": r.keep_alive_polls,
        "fsevents_reinstate": r.fsevents_reinstate,
        "polls_elapsed": r.polls_elapsed,
        "failure": r.failure,
        "source": r.source,
    })
}

fn daemon_health_to_json(h: &DaemonHealth) -> serde_json::Value {
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
        "reaper": h.reaper.as_ref().map(|r| serde_json::json!({
            "armed": r.armed,
            "visibility": r.visibility,
            "sweeps": r.sweeps,
            "reaped": r.reaped,
            "kill_denied": r.kill_denied,
        })),
        "verdicts": h.verdicts.iter().map(|v| serde_json::json!({
            "level": v.level,
            "message": v.message,
        })).collect::<Vec<_>>(),
    })
}

/// Recognised keys of `bc_introspect`'s `options_json` argument.
#[derive(serde::Deserialize, Default)]
struct IntrospectOptions {
    duration_secs: Option<u64>,
}

fn introspect_subject_from_str(name: &str) -> Result<IntrospectSubject, FfiError> {
    match name {
        "daemon" => Ok(IntrospectSubject::Daemon),
        "providers" => Ok(IntrospectSubject::Providers),
        "config" => Ok(IntrospectSubject::Config),
        "cache" => Ok(IntrospectSubject::Cache),
        "lifecycle" => Ok(IntrospectSubject::Lifecycle),
        "watches" => Ok(IntrospectSubject::Watches),
        "timers" => Ok(IntrospectSubject::Timers),
        "demand" => Ok(IntrospectSubject::Demand),
        "procs" => Ok(IntrospectSubject::Procs),
        other => Err(FfiError::new(
            ErrorKind::ParseError,
            format!("unknown introspect subject: {other}"),
        )),
    }
}

/// Query a single key, raw. `path` is nullable (global providers). `flags` is
/// a bitmask of [`BC_GET_FORCE`] / [`BC_GET_WAIT`]; any other bit set yields
/// a `bad_flags` envelope.
///
/// The envelope's `data` object is the node as the daemon holds it — nothing
/// rendered; `bc_resolve` is what renders a template into a value:
///
/// | node               | `data`   | `age_ms`/`stale` | `template`        | `rest`      |
/// |--------------------|----------|------------------|-------------------|-------------|
/// | value (a hit)      | value    | set              | only if templated | only if set |
/// | template only      | `null`   | `null`           | string or tree    | only if set |
/// | nothing (a miss)   | `null`   | `null`           | absent            | absent      |
///
/// A template-only node is not a miss: something is stored there, and it
/// has no value until rendered.
///
/// # Safety
/// `client` must be a valid pointer from [`bc_client_new`]; `key` must be
/// non-null and NUL-terminated; `path` must be NULL or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_get(
    client: *mut BcClient,
    key: *const c_char,
    path: *const c_char,
    flags: u32,
) -> *mut c_char {
    call_ffi(move || {
        let c = unsafe { client_ref(client) }?;
        check_get_flags(flags)?;
        let key = unsafe { required_str(key, "key") }?;
        let path = unsafe { optional_str(path) }?;
        let force = flags & BC_GET_FORCE != 0;
        let wait = flags & BC_GET_WAIT != 0;
        let result = c.get_with_flags(key, path, force, wait)?;
        Ok(combresult_to_json(result))
    })
}

/// Store data into the cache as static data. `json_data` must parse as JSON.
/// `ttl` and `path` are nullable.
///
/// # Safety
/// `client` must be a valid pointer from [`bc_client_new`]; `key` and
/// `json_data` must be non-null and NUL-terminated; `ttl` and `path` must be
/// NULL or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_put(
    client: *mut BcClient,
    key: *const c_char,
    json_data: *const c_char,
    ttl: *const c_char,
    path: *const c_char,
) -> *mut c_char {
    call_ffi(move || {
        let c = unsafe { client_ref(client) }?;
        let key = unsafe { required_str(key, "key") }?;
        let json_data = unsafe { required_str(json_data, "json_data") }?;
        let data: serde_json::Value = serde_json::from_str(json_data).map_err(|e| {
            FfiError::new(ErrorKind::ParseError, format!("malformed json_data: {e}"))
        })?;
        let ttl = unsafe { optional_str(ttl) }?;
        let path = unsafe { optional_str(path) }?;
        c.put(key, data, ttl, path)?;
        Ok(serde_json::Value::Null)
    })
}

/// Clear the cached entry for a static-data key without dropping the
/// registry entry. `path` is nullable.
///
/// # Safety
/// `client` must be a valid pointer from [`bc_client_new`]; `key` must be
/// non-null and NUL-terminated; `path` must be NULL or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_put_null(
    client: *mut BcClient,
    key: *const c_char,
    path: *const c_char,
) -> *mut c_char {
    call_ffi(move || {
        let c = unsafe { client_ref(client) }?;
        let key = unsafe { required_str(key, "key") }?;
        let path = unsafe { optional_str(path) }?;
        c.put_null(key, path)?;
        Ok(serde_json::Value::Null)
    })
}

/// Re-execute the Sources `key` demands — every Source of the namespace for
/// a bare `provider`, only the one owning the field for `provider.field` —
/// each at the cache entry its own scope keys. `path` is nullable. A key
/// naming no Source (a static namespace, a facet, an unknown name, or a
/// field no Source owns) is an error: `nothing to refresh: <key> has no
/// sources`.
///
/// # Safety
/// `client` must be a valid pointer from [`bc_client_new`]; `key` must be
/// non-null and NUL-terminated; `path` must be NULL or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_refresh(
    client: *mut BcClient,
    key: *const c_char,
    path: *const c_char,
) -> *mut c_char {
    call_ffi(move || {
        let c = unsafe { client_ref(client) }?;
        let key = unsafe { required_str(key, "key") }?;
        let path = unsafe { optional_str(path) }?;
        c.refresh(key, path)?;
        Ok(serde_json::Value::Null)
    })
}

/// List all cache entries currently held by the daemon.
///
/// # Safety
/// `client` must be a valid pointer from [`bc_client_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_status(client: *mut BcClient) -> *mut c_char {
    call_ffi(move || {
        let c = unsafe { client_ref(client) }?;
        let rows = c.status()?;
        let arr: Vec<serde_json::Value> = rows.iter().map(cache_row_to_json).collect();
        Ok(serde_json::Value::Array(arr))
    })
}

/// Run an introspect query. `options_json` is nullable; its only recognised
/// key is `duration_secs`, consulted by the `procs` subject only.
///
/// # Safety
/// `client` must be a valid pointer from [`bc_client_new`]; `subject` must
/// be non-null and NUL-terminated; `options_json` must be NULL or
/// NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_introspect(
    client: *mut BcClient,
    subject: *const c_char,
    options_json: *const c_char,
) -> *mut c_char {
    call_ffi(move || {
        let c = unsafe { client_ref(client) }?;
        let subject = unsafe { required_str(subject, "subject") }?;
        let subject = introspect_subject_from_str(subject)?;
        let options_json = unsafe { optional_str(options_json) }?;
        let options: IntrospectOptions = match options_json {
            Some(s) => serde_json::from_str(s).map_err(|e| {
                FfiError::new(
                    ErrorKind::ParseError,
                    format!("malformed options_json: {e}"),
                )
            })?,
            None => IntrospectOptions::default(),
        };
        match c.introspect(subject, options.duration_secs)? {
            IntrospectResponse::Daemon(health) => Ok(daemon_health_to_json(&health)),
            IntrospectResponse::Other(v) => Ok(v),
        }
    })
}

/// Ask the daemon for its protocol and build versions.
///
/// # Safety
/// `client` must be a valid pointer from [`bc_client_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_hello(client: *mut BcClient) -> *mut c_char {
    call_ffi(move || {
        let c = unsafe { client_ref(client) }?;
        let info = c.hello()?;
        Ok(serde_json::json!({
            "protocol_version": info.protocol_version,
            "daemon_version": info.daemon_version,
        }))
    })
}

// ── Resolution (Task 3.5) ──────────────────────────────────────────────

/// Parses a nullable `env_json` argument. `None` (NULL) means "no env
/// supplied" — every `env.*` reference then resolves to `""`, matching the
/// evaluator's own miss semantics on an empty map. Values must be strings.
fn parse_env_json(json: Option<&str>) -> Result<HashMap<String, String>, FfiError> {
    match json {
        None => Ok(HashMap::new()),
        Some(s) => serde_json::from_str(s)
            .map_err(|e| FfiError::new(ErrorKind::ParseError, format!("malformed env_json: {e}"))),
    }
}

/// A resolution failure as an envelope error.
///
/// A template that does not compile, cycles or runs past the depth limit is
/// configuration someone has to go and edit — the caller's input being
/// malformed, `parse_error`. A failure while rendering (`exec()` disabled or
/// failed) is `server_error`, and so is the daemon refusing the key itself,
/// in the daemon's own words. A transport failure keeps the kind its
/// `CombError` gave it — `daemon_not_running`, `timeout`, … — which
/// [`ClientFetch`] recorded when it reported the failure.
fn resolve_error(e: ResolveError, fetch: &mut ClientFetch<'_>) -> FfiError {
    match e {
        ResolveError::Config(m) => FfiError::new(ErrorKind::ParseError, m),
        ResolveError::Runtime(m) => FfiError::new(ErrorKind::ServerError, m),
        ResolveError::Fetch(FetchError::Rejected(m)) => FfiError::new(ErrorKind::ServerError, m),
        ResolveError::Fetch(FetchError::Transport(_)) => fetch
            .transport
            .take()
            .map(FfiError::from)
            .expect("ClientFetch records the CombError behind every Transport"),
    }
}

/// Resolve `key` to the value `comb get key` prints at `cwd`, through
/// [`libbeachcomber::resolve`]: the daemon holds the templates
/// (`<provider>:template`, seeded from configuration or `put`), and this
/// client renders them with `env_json` as the caller's environment. A plain
/// field is its cached value; a templated one is rendered; a bare provider
/// is the whole merged namespace, as `comb get provider -f json`. A miss is
/// `data: null`.
///
/// `cwd` is required: NULL returns an error envelope rather than falling
/// back to the process's own working directory — this library must never
/// read ambient state on the caller's behalf. Every fetch the resolution
/// makes addresses `cwd`, the same convention `comb get --path` follows.
/// `env_json` is nullable, meaning "no environment": every `env.*` is `""`.
/// `exec()` inside a template runs only on a client created with
/// `{"exec":true}`; otherwise it fails with `exec() is disabled in this
/// context`.
///
/// # Safety
/// `client` must be a valid pointer from [`bc_client_new`]; `key` and `cwd`
/// must be non-null and NUL-terminated; `env_json` must be NULL or
/// NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_resolve(
    client: *mut BcClient,
    key: *const c_char,
    cwd: *const c_char,
    env_json: *const c_char,
) -> *mut c_char {
    call_ffi(move || {
        let c = unsafe { client_ref(client) }?;
        let key = unsafe { required_str(key, "key") }?;
        let cwd = unsafe { required_str(cwd, "cwd") }?;
        let env_vars = parse_env_json(unsafe { optional_str(env_json) }?)?;
        let inputs = Inputs {
            cwd,
            env: &env_vars,
            slot: Slot::Path(cwd.to_string()),
            force: false,
            wait: false,
            allow_exec: c.allow_exec(),
        };
        let mut fetch = ClientFetch::new(c);
        match resolve::resolve(key, &inputs, &mut fetch) {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Ok(serde_json::Value::Null),
            Err(e) => Err(resolve_error(e, &mut fetch)),
        }
    })
}

/// Render a source that is stored nowhere, every ref resolved exactly as it
/// would be inside a stored template — so `bc_eval("{{ p.f }}")` and
/// `bc_resolve("p.f")` agree by construction. A source that is exactly one
/// `{{ expr }}` tag keeps the expression's natural type; anything else
/// (literal text, several tags, a source with no tags at all) is a string.
/// A missing ref — an absent `env.*`, a daemon miss, a key the daemon
/// rejects — is falsy at any depth, not an error.
///
/// `cwd`, `env_json` and `exec()` behave as for [`bc_resolve`].
///
/// # Safety
/// `client` must be a valid pointer from [`bc_client_new`]; `template_str`
/// and `cwd` must be non-null and NUL-terminated; `env_json` must be NULL
/// or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_eval(
    client: *mut BcClient,
    template_str: *const c_char,
    cwd: *const c_char,
    env_json: *const c_char,
) -> *mut c_char {
    call_ffi(move || {
        let c = unsafe { client_ref(client) }?;
        let template_str = unsafe { required_str(template_str, "template_str") }?;
        let cwd = unsafe { required_str(cwd, "cwd") }?;
        let env_vars = parse_env_json(unsafe { optional_str(env_json) }?)?;
        let inputs = Inputs {
            cwd,
            env: &env_vars,
            slot: Slot::Path(cwd.to_string()),
            force: false,
            wait: false,
            allow_exec: c.allow_exec(),
        };
        let mut fetch = ClientFetch::new(c);
        resolve::eval(template_str, &inputs, &mut fetch).map_err(|e| resolve_error(e, &mut fetch))
    })
}

// ── Sessions (Task 3.6) ─────────────────────────────────────────────────

/// Opaque handle to a persistent connection for multiple queries.
///
/// The connection is guarded by an internal mutex: a caller that finds it
/// already locked gets a `kind: "busy"` envelope immediately rather than
/// blocking or interleaving its request with another caller's on the same
/// socket. `state` also carries a deferred construction error, the same
/// pattern [`BcClient`] uses — [`bc_session_open`] never returns NULL for a
/// reason short of allocation failure.
pub struct BcSession {
    state: Mutex<Result<libbeachcomber::Session, FfiError>>,
}

/// Locks `session`'s mutex, translating an already-held lock into a `busy`
/// envelope rather than blocking. A poisoned mutex (a previous call panicked
/// while holding it) is recovered rather than treated as busy: the panic
/// that poisoned it was already turned into its own envelope by [`call_ffi`]
/// on that earlier call, so the session's `Result` is still a valid value to
/// read here.
///
/// # Safety
/// `session` must be a valid, non-null pointer previously returned by
/// [`bc_session_open`], not yet closed.
unsafe fn session_lock<'a>(
    session: *mut BcSession,
) -> Result<std::sync::MutexGuard<'a, Result<libbeachcomber::Session, FfiError>>, FfiError> {
    let handle = unsafe { &*session };
    match handle.state.try_lock() {
        Ok(guard) => Ok(guard),
        Err(std::sync::TryLockError::WouldBlock) => Err(FfiError::new(
            ErrorKind::Busy,
            "session handle is in use by another caller",
        )),
        Err(std::sync::TryLockError::Poisoned(poisoned)) => Ok(poisoned.into_inner()),
    }
}

/// Opens a persistent session on `client`'s connection. Never returns NULL:
/// a `client` whose own construction failed, or a daemon that is
/// unreachable when the underlying connection is attempted, is recorded on
/// the handle and surfaced on the session's first operation instead.
///
/// # Safety
/// `client` must be a valid, non-null pointer previously returned by
/// [`bc_client_new`], not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_session_open(client: *mut BcClient) -> *mut BcSession {
    let state = (|| {
        let c = unsafe { client_ref(client) }?;
        c.session().map_err(FfiError::from)
    })();
    Box::into_raw(Box::new(BcSession {
        state: Mutex::new(state),
    }))
}

/// Closes and frees a session handle. Null-safe.
///
/// # Safety
/// `session` must be either NULL or a pointer previously returned by
/// [`bc_session_open`], not yet closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_session_close(session: *mut BcSession) {
    if session.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(session) });
}

/// Query a single key on this session's persistent connection, raw. `flags`
/// is the same [`BC_GET_FORCE`] / [`BC_GET_WAIT`] bitmask [`bc_get`] takes,
/// and the envelope's `data` object has the same three shapes:
///
/// | node               | `data`   | `age_ms`/`stale` | `template`        | `rest`      |
/// |--------------------|----------|------------------|-------------------|-------------|
/// | value (a hit)      | value    | set              | only if templated | only if set |
/// | template only      | `null`   | `null`           | string or tree    | only if set |
/// | nothing (a miss)   | `null`   | `null`           | absent            | absent      |
///
/// # Safety
/// `session` must be a valid pointer from [`bc_session_open`]; `key` must
/// be non-null and NUL-terminated; `path` must be NULL or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_session_get(
    session: *mut BcSession,
    key: *const c_char,
    path: *const c_char,
    flags: u32,
) -> *mut c_char {
    call_ffi(move || {
        check_get_flags(flags)?;
        let key = unsafe { required_str(key, "key") }?;
        let path = unsafe { optional_str(path) }?;
        let mut guard = unsafe { session_lock(session) }?;
        let sess = guard.as_mut().map_err(|e| e.clone())?;
        let force = flags & BC_GET_FORCE != 0;
        let wait = flags & BC_GET_WAIT != 0;
        let result = sess.get_with_flags(key, path, force, wait)?;
        Ok(combresult_to_json(result))
    })
}

/// Store data into the cache as static data on this session's persistent
/// connection. `json_data` must parse as JSON; `ttl` and `path` are
/// nullable.
///
/// # Safety
/// `session` must be a valid pointer from [`bc_session_open`]; `key` and
/// `json_data` must be non-null and NUL-terminated; `ttl` and `path` must be
/// NULL or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_session_put(
    session: *mut BcSession,
    key: *const c_char,
    json_data: *const c_char,
    ttl: *const c_char,
    path: *const c_char,
) -> *mut c_char {
    call_ffi(move || {
        let key = unsafe { required_str(key, "key") }?;
        let json_data = unsafe { required_str(json_data, "json_data") }?;
        let data: serde_json::Value = serde_json::from_str(json_data).map_err(|e| {
            FfiError::new(ErrorKind::ParseError, format!("malformed json_data: {e}"))
        })?;
        let ttl = unsafe { optional_str(ttl) }?;
        let path = unsafe { optional_str(path) }?;
        let mut guard = unsafe { session_lock(session) }?;
        let sess = guard.as_mut().map_err(|e| e.clone())?;
        sess.put(key, data, ttl, path)?;
        Ok(serde_json::Value::Null)
    })
}

/// Sets connection context on this session so subsequent queries don't need
/// an explicit `path`.
///
/// # Safety
/// `session` must be a valid pointer from [`bc_session_open`]; `path` must
/// be non-null and NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_session_set_context(
    session: *mut BcSession,
    path: *const c_char,
) -> *mut c_char {
    call_ffi(move || {
        let path = unsafe { required_str(path, "path") }?;
        let mut guard = unsafe { session_lock(session) }?;
        let sess = guard.as_mut().map_err(|e| e.clone())?;
        sess.set_context(path)?;
        Ok(serde_json::Value::Null)
    })
}

// ── Watch (Task 3.7) ────────────────────────────────────────────────────

/// How often [`bc_watch_next`]'s poll loop re-checks the cancellation flag
/// (and, for `timeout_ms > 0`, re-checks the deadline) between read
/// attempts. Bounds how long a call may block past [`bc_watch_cancel`]
/// being invoked on another thread: the underlying blocking read is never
/// asked to wait longer than this in one attempt, so cancellation is
/// noticed within roughly one tick even under `timeout_ms = -1`.
const WATCH_POLL_TICK: Duration = Duration::from_millis(50);

/// Opaque handle to a watch stream.
///
/// Guarded the same way [`BcSession`] is: an internal mutex that
/// [`bc_watch_next`] takes with `try_lock`, yielding `kind: "busy"` for a
/// concurrent caller rather than blocking. `cancelled` is deliberately
/// **outside** that mutex — [`bc_watch_cancel`] is the one call in this
/// crate documented as safe to invoke from another thread while an op is in
/// flight, and it must not itself block on the lock a pending
/// [`bc_watch_next`] is holding.
pub struct BcWatch {
    stream: Mutex<Result<libbeachcomber::WatchStream, FfiError>>,
    cancelled: AtomicBool,
}

/// Opens a watch on `key`. Returns NULL only on allocation failure — any
/// other failure (a `client` whose own construction failed, or the watch
/// request itself failing) is recorded on the handle and surfaced on the
/// first [`bc_watch_next`] call instead.
///
/// # Safety
/// `client` must be a valid, non-null pointer from [`bc_client_new`]; `key`
/// must be non-null and NUL-terminated; `path` must be NULL or
/// NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_watch_open(
    client: *mut BcClient,
    key: *const c_char,
    path: *const c_char,
) -> *mut BcWatch {
    let state = (|| {
        let c = unsafe { client_ref(client) }?;
        let key = unsafe { required_str(key, "key") }?;
        let path = unsafe { optional_str(path) }?;
        c.watch(key, path).map_err(FfiError::from)
    })();
    Box::into_raw(Box::new(BcWatch {
        stream: Mutex::new(state),
        cancelled: AtomicBool::new(false),
    }))
}

fn is_timeout_like(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

fn watch_event_to_json(e: &libbeachcomber::WatchEvent) -> serde_json::Value {
    serde_json::json!({
        "data": e.data.as_ref().map(|d| d.as_value().clone()),
        "age_ms": e.age_ms,
        "stale": e.stale,
    })
}

/// Waits for the next event on `w`. `timeout_ms`: `-1` blocks indefinitely,
/// `0` polls (returns immediately if nothing is ready), `>0` waits that
/// long. Distinguishes five outcomes, all machine-readable without string
/// matching (see [`WatchOutcome`]): `event`, `timeout`, `eof`, `cancelled`
/// (via `ok:true` plus an `outcome` field), and `error` (the ordinary
/// `ok:false` envelope, already machine-readable via `error.kind`).
/// `error` is reserved for the daemon actively rejecting the watched key
/// (a malformed response line, or the `ServerError` a malformed or
/// unregistered key produces — an address nothing answers is a miss frame,
/// not an error) — any lower-level socket failure (a reset
/// connection, or even a failing `set_read_timeout`, observed on macOS as
/// `EINVAL` from `setsockopt` on an already-reset unix socket rather than
/// the loss surfacing on the next read) is treated as `eof`: from a
/// caller's point of view the stream is over either way, and a binding
/// should not have to special-case platform-specific socket-teardown
/// quirks as a distinct failure mode.
///
/// Internally this holds the socket's read timeout to at most
/// [`WATCH_POLL_TICK`] per attempt and loops, re-checking the cancellation
/// flag and (for `timeout_ms > 0`) the deadline between attempts, rather
/// than issuing one read for the full requested wait — that is what lets
/// [`bc_watch_cancel`], called from another thread mid-wait, unblock a
/// pending call within about one tick instead of only at the next natural
/// wakeup. A timed-out attempt's partially-read bytes (if a line arrived
/// split across attempts) are preserved across retries by
/// [`libbeachcomber::WatchStream::read_line_buffered`] rather than
/// discarded, so this cannot silently drop half of an event.
///
/// # Safety
/// `w` must be a valid, non-null pointer previously returned by
/// [`bc_watch_open`], not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_watch_next(w: *mut BcWatch, timeout_ms: i32) -> *mut c_char {
    call_watch_next(move || {
        let handle = unsafe { &*w };
        if handle.cancelled.load(Ordering::SeqCst) {
            return Ok(WatchOutcome::Cancelled);
        }
        let mut guard = match handle.stream.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err(FfiError::new(
                    ErrorKind::Busy,
                    "watch handle is in use by another caller",
                ));
            }
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        let stream = guard.as_mut().map_err(|e| e.clone())?;

        // `timeout_ms < 0` (canonically -1) blocks indefinitely: no deadline.
        let deadline =
            (timeout_ms > 0).then(|| Instant::now() + Duration::from_millis(timeout_ms as u64));

        let mut line = String::new();
        loop {
            if handle.cancelled.load(Ordering::SeqCst) {
                return Ok(WatchOutcome::Cancelled);
            }
            let attempt = if timeout_ms == 0 {
                // A single minimal-wait attempt: "ready right now, or not".
                Duration::from_millis(1)
            } else if let Some(deadline) = deadline {
                let now = Instant::now();
                if now >= deadline {
                    return Ok(WatchOutcome::Timeout);
                }
                (deadline - now).min(WATCH_POLL_TICK)
            } else {
                WATCH_POLL_TICK
            };
            // A socket that just lost its peer can fail to set a timeout at
            // all (observed on macOS as `EINVAL` from `setsockopt` on an
            // abruptly-reset unix socket) rather than surfacing the loss on
            // the next read. Either way there is no connection left to wait
            // on, so this is the same outcome a clean `read() == 0` would
            // report: `eof`, not a distinct application-level `error`.
            if stream.set_read_timeout(Some(attempt)).is_err() {
                return Ok(WatchOutcome::Eof);
            }

            match stream.read_line_buffered(&mut line) {
                Ok(0) => return Ok(WatchOutcome::Eof),
                Ok(_) if line.ends_with('\n') => {
                    let event =
                        libbeachcomber::WatchStream::parse_line(&line).map_err(FfiError::from)?;
                    return Ok(WatchOutcome::Event(watch_event_to_json(&event)));
                }
                // A partial final line with no trailing newline before EOF:
                // there is no complete event to report.
                Ok(_) => return Ok(WatchOutcome::Eof),
                Err(e) if is_timeout_like(&e) => {
                    if timeout_ms == 0 {
                        return Ok(WatchOutcome::Timeout);
                    }
                    continue;
                }
                // Any other read failure (e.g. a reset connection) likewise
                // means the stream is over, not that this particular call
                // was rejected.
                Err(_) => return Ok(WatchOutcome::Eof),
            }
        }
    })
}

/// Cancels a pending or future [`bc_watch_next`] call on `w`. Null-safe.
/// The sole function in this crate documented as safe to call from a
/// different thread than the one driving `w`'s other operations — it never
/// takes `w`'s internal lock, only sets an atomic flag a pending
/// `bc_watch_next` polls for. A `bc_watch_next` call already in flight
/// observes it within about [`WATCH_POLL_TICK`]; every call after this one
/// observes it immediately.
///
/// # Safety
/// `w` must be either NULL or a pointer previously returned by
/// [`bc_watch_open`], not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_watch_cancel(w: *mut BcWatch) {
    if w.is_null() {
        return;
    }
    let handle = unsafe { &*w };
    handle.cancelled.store(true, Ordering::SeqCst);
}

/// Frees a watch handle. Null-safe.
///
/// # Safety
/// `w` must be either NULL or a pointer previously returned by
/// [`bc_watch_open`], not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_watch_free(w: *mut BcWatch) {
    if w.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(w) });
}
