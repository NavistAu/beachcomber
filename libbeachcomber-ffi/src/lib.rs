//! C ABI surface for `libbeachcomber`. This crate contains no logic of its
//! own — every `extern "C"` entry point is a thin wrapper delegating to
//! `libbeachcomber`.

use std::ffi::{CStr, CString, c_char};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub mod envelope;

use envelope::{ErrorKind, FfiError, WatchOutcome, call_ffi, call_watch_next};
use libbeachcomber::resolve::{self, FetchError, Inputs, ResolveError, Slot};
use libbeachcomber::watch::{ResolvedWatch, WatchError, WatchOutcome as LibWatchOutcome};
use libbeachcomber::{
    CacheRow, ClientFetch, CombResult, DaemonHealth, IntrospectResponse, IntrospectSubject,
};
use std::collections::HashMap;

/// Opaque client handle returned by [`bc_client_new`].
///
/// Construction never fails from the caller's point of view: a malformed
/// `options_json` is recorded here and surfaced on the handle's first
/// operation instead of failing the constructor.
///
/// The client sits behind an [`Arc`] because a watch outlives the call that
/// opened it: [`bc_watch_open`] hands a share to the
/// [`ResolvedWatch`](libbeachcomber::watch::ResolvedWatch) it returns, so a
/// `BcWatch` keeps its client alive rather than resting on a
/// "free the client last" contract no other handle has.
pub struct BcClient {
    state: Result<Arc<libbeachcomber::Client>, FfiError>,
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
    let state = unsafe { build_client(options_json) }.map(Arc::new);
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
    handle
        .state
        .as_ref()
        .map(|client| &**client)
        .map_err(|e| e.clone())
}

/// A share of the handle's underlying client, for an operation whose result
/// outlives the call ([`bc_watch_open`]), or its deferred construction error.
///
/// # Safety
/// `client` must be a valid, non-null pointer previously returned by
/// [`bc_client_new`], not yet freed.
unsafe fn client_arc(client: *mut BcClient) -> Result<Arc<libbeachcomber::Client>, FfiError> {
    let handle = unsafe { &*client };
    handle.state.as_ref().map(Arc::clone).map_err(|e| e.clone())
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
        "poll_count": r.poll_count,
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

/// Re-execute the Sources `key` demands — every Source of the namespace for
/// a bare `provider`, only the one owning the field for `provider.field` —
/// each at the instance that answers for `path`. `path` is nullable. A key
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

/// The directory a resolution addresses: NULL is the global entry, as
/// `comb get --global` is; anything else is normalised as every request path
/// is ([`libbeachcomber::path::normalise_here`]): `~` is the caller's home and
/// a relative path is against the caller's working directory.
///
/// # Safety
/// `cwd` must be NULL or NUL-terminated.
unsafe fn resolution_dir(cwd: *const c_char) -> Result<(String, Slot), FfiError> {
    if cwd.is_null() {
        return Ok((String::new(), Slot::Global));
    }
    let dir = unsafe { required_str(cwd, "cwd") }?;
    let dir = libbeachcomber::path::normalise_here(Some(dir))
        .map_err(|e| FfiError::new(ErrorKind::ParseError, e.to_string()))?;
    Ok((dir.clone(), Slot::Path(dir)))
}

/// Resolve `key` to the value `comb get key` prints at `cwd`, through
/// [`libbeachcomber::resolve`]: the daemon holds the templates
/// (`<provider>:template`, seeded from configuration or `put`), and this
/// client renders them with `env_json` as the caller's environment. A plain
/// field is its cached value; a templated one is rendered; a bare provider
/// is the whole merged namespace, as `comb get provider -f json`. A miss is
/// `data: null`.
///
/// `cwd` is the directory the resolution addresses, read as `comb get --path`
/// reads one: `~` is the home directory and a relative path is against the
/// caller's working directory. NULL addresses the global entry, as
/// `comb get --global` does. Every fetch the resolution makes addresses that
/// directory, and a template's `exec`, `lup` and `read` run in it (in
/// `$HOME` from `env_json` for the global entry).
/// `env_json` is nullable, meaning "no environment": every `env.*` is absent.
/// `exec()` inside a template runs only on a client created with
/// `{"exec":true}`; otherwise it fails with `exec() is disabled in this
/// context`.
///
/// # Safety
/// `client` must be a valid pointer from [`bc_client_new`]; `key` must be
/// non-null and NUL-terminated; `cwd` and `env_json` must be NULL or
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
        let (cwd, slot) = unsafe { resolution_dir(cwd) }?;
        let env_vars = parse_env_json(unsafe { optional_str(env_json) }?)?;
        let inputs = Inputs {
            cwd: &cwd,
            env: &env_vars,
            slot,
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
/// rejects — is falsy at any depth, not an error. A one-tag source whose
/// value is `none` or undefined is absent: `data: null`, as a miss is for
/// [`bc_resolve`].
///
/// `cwd`, `env_json` and `exec()` behave as for [`bc_resolve`].
///
/// # Safety
/// `client` must be a valid pointer from [`bc_client_new`]; `template_str`
/// must be non-null and NUL-terminated; `cwd` and `env_json` must be NULL or
/// NUL-terminated.
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
        let (cwd, slot) = unsafe { resolution_dir(cwd) }?;
        let env_vars = parse_env_json(unsafe { optional_str(env_json) }?)?;
        let inputs = Inputs {
            cwd: &cwd,
            env: &env_vars,
            slot,
            force: false,
            wait: false,
            allow_exec: c.allow_exec(),
        };
        let mut fetch = ClientFetch::new(c);
        match resolve::eval(template_str, &inputs, &mut fetch) {
            Ok(value) => Ok(value.unwrap_or(serde_json::Value::Null)),
            Err(e) => Err(resolve_error(e, &mut fetch)),
        }
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

/// Re-execute the Sources `key` demands, as [`bc_refresh`], at `path` — or,
/// when `path` is NULL, at this session's directory ([`bc_session_set_context`]).
///
/// # Safety
/// `session` must be a valid pointer from [`bc_session_open`]; `key` must be
/// non-null and NUL-terminated; `path` must be NULL or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_session_refresh(
    session: *mut BcSession,
    key: *const c_char,
    path: *const c_char,
) -> *mut c_char {
    call_ffi(move || {
        let key = unsafe { required_str(key, "key") }?;
        let path = unsafe { optional_str(path) }?;
        let mut guard = unsafe { session_lock(session) }?;
        let sess = guard.as_mut().map_err(|e| e.clone())?;
        sess.refresh(key, path)?;
        Ok(serde_json::Value::Null)
    })
}

/// Set this session's directory: every later `bc_session_get`,
/// `bc_session_put` and `bc_session_refresh` that passes a NULL `path`
/// addresses it. The session keeps it and sends it with each request; it is
/// read as every path is (`~` is home, a relative path is against the
/// process's working directory).
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

/// A watch failure as an envelope error.
///
/// The same classification [`resolve_error`] makes of a resolution, plus the
/// two failures only a watch has: a key that cannot be watched at all, which
/// is the caller's input being wrong (`parse_error`, in the library's own
/// words — no daemon was involved in deciding it), and a transport failure,
/// which keeps the kind its `CombError` gave it.
fn watch_error(e: WatchError) -> FfiError {
    match e {
        WatchError::EnvKey(m) => FfiError::new(ErrorKind::ParseError, m),
        WatchError::Resolve(ResolveError::Config(m)) => FfiError::new(ErrorKind::ParseError, m),
        WatchError::Resolve(ResolveError::Runtime(m)) => FfiError::new(ErrorKind::ServerError, m),
        WatchError::Resolve(ResolveError::Fetch(FetchError::Rejected(m))) => {
            FfiError::new(ErrorKind::ServerError, m)
        }
        WatchError::Resolve(ResolveError::Fetch(FetchError::Transport(m))) => {
            FfiError::new(ErrorKind::IoError, m)
        }
        WatchError::Transport(e) => FfiError::from(e),
    }
}

/// Opaque handle to a watch.
///
/// Guarded the same way [`BcSession`] is: an internal mutex that
/// [`bc_watch_next`] takes with `try_lock`, yielding `kind: "busy"` for a
/// concurrent caller rather than blocking. `cancelled` is deliberately
/// **outside** that mutex — [`bc_watch_cancel`] is the one call in this
/// crate documented as safe to invoke from another thread while an op is in
/// flight, and it must not itself block on the lock a pending
/// [`bc_watch_next`] is holding. It is the watch's own
/// [`cancel_handle`](ResolvedWatch::cancel_handle), not a second flag beside
/// it: setting it is what ends a wait already inside
/// [`ResolvedWatch::next`], which is where a blocked call actually sits.
// `ended` latches the terminal state. A watch that has reported `eof` has
// nothing left that could ever answer, and one that failed ended on that
// failure — so neither resumes polling on the next call: the `eof` comes
// back from `ended`, and the failure comes back because it replaced the
// `ResolvedWatch` in `watch`.
pub struct BcWatch {
    watch: Mutex<Result<ResolvedWatch<'static>, FfiError>>,
    cancelled: Arc<AtomicBool>,
    ended: AtomicBool,
}

/// Opens a watch on the *value* of `key` — what `bc_resolve(key, cwd,
/// env_json)` answers — and emits it again whenever it changes. The fan-in
/// underneath subscribes to every key the resolution read, and a write to a
/// namespace's template notifies that namespace, so a template edited under
/// the watch fires it exactly as a changed cache record does.
///
/// `cwd` and `env_json` mean what they mean for [`bc_resolve`]: `cwd` is
/// both the directory every subscription is opened at and the one every
/// re-resolution runs in, and NULL watches the global entry. `env_json` is
/// nullable, meaning "no environment": every `env.*` is absent.
/// `exec()` inside a template runs only on a client created with
/// `{"exec":true}`.
///
/// Returns NULL only on allocation failure — any other failure (a `client`
/// whose own construction failed, a key that cannot be watched, or the
/// opening resolution failing) is recorded on the handle and surfaced on the
/// first [`bc_watch_next`] call instead.
///
/// # Safety
/// `client` must be a valid, non-null pointer from [`bc_client_new`]; `key`
/// must be non-null and NUL-terminated; `cwd` and `env_json` must be NULL or
/// NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_watch_open(
    client: *mut BcClient,
    key: *const c_char,
    cwd: *const c_char,
    env_json: *const c_char,
) -> *mut BcWatch {
    let state = (|| {
        let c = unsafe { client_arc(client) }?;
        let key = unsafe { required_str(key, "key") }?;
        let (cwd, slot) = unsafe { resolution_dir(cwd) }?;
        let env_vars = parse_env_json(unsafe { optional_str(env_json) }?)?;
        let inputs = Inputs {
            cwd: &cwd,
            env: &env_vars,
            slot,
            // A watch is a subscription, not a demand: it never forces an
            // execution or waits on one.
            force: false,
            wait: false,
            allow_exec: c.allow_exec(),
        };
        ResolvedWatch::open_owned(c, key, &inputs).map_err(watch_error)
    })();
    // A watch that never opened has no wait to interrupt; the flag exists
    // only so `bc_watch_cancel` stays null-safe-simple on every handle.
    let cancelled = match &state {
        Ok(watch) => watch.cancel_handle(),
        Err(_) => Arc::new(AtomicBool::new(false)),
    };
    Box::into_raw(Box::new(BcWatch {
        watch: Mutex::new(state),
        cancelled,
        ended: AtomicBool::new(false),
    }))
}

/// Waits for the next value on `w`. `timeout_ms`: `-1` blocks indefinitely,
/// `0` polls (one pass over the open subscriptions, then `timeout` if none
/// of them had anything), `>0` waits that long. Distinguishes five outcomes,
/// all machine-readable without string matching (see [`WatchOutcome`]):
/// `event` — whose `data` is the resolved value, `null` for a miss —
/// `timeout`, `eof`, `cancelled` (via `ok:true` plus an `outcome` field),
/// and `error` (the ordinary `ok:false` envelope, already machine-readable
/// via `error.kind`).
///
/// Only a change is an event: a frame whose re-resolution comes out the same
/// value keeps the wait going. The first call on a provider key answers with
/// the value as it was when the watch opened, without waiting for a change —
/// but do not assume it: on a facet key the first value comes over the wire,
/// so the first call can just as well be `timeout`, `eof`, or the daemon's
/// refusal of the key.
///
/// `error` is the daemon actively rejecting the watched key, or a template
/// the resolution cannot render; any lower-level socket failure (a reset
/// connection, or even a failing `set_read_timeout`, observed on macOS as
/// `EINVAL` from `setsockopt` on an already-reset unix socket rather than
/// the loss surfacing on the next read) is `eof`: from a caller's point of
/// view the stream is over either way, and a binding should not have to
/// special-case platform-specific socket-teardown quirks as a distinct
/// failure mode.
///
/// Both terminal outcomes latch: after `eof`, or after an `error`, every
/// later call reports the same thing rather than resuming the poll.
///
/// The wait itself is [`ResolvedWatch::next`]'s — a bounded read per open
/// subscription in turn, re-checking the cancellation flag between passes,
/// rather than one read for the full requested wait. That is what lets
/// [`bc_watch_cancel`], called from another thread mid-wait, unblock a
/// pending call within about one poll tick instead of only at the next
/// natural wakeup.
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
        if handle.ended.load(Ordering::SeqCst) {
            return Ok(WatchOutcome::Eof);
        }
        let mut guard = match handle.watch.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err(FfiError::new(
                    ErrorKind::Busy,
                    "watch handle is in use by another caller",
                ));
            }
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };

        // `timeout_ms < 0` (canonically -1) blocks indefinitely.
        let timeout = (timeout_ms >= 0).then(|| Duration::from_millis(timeout_ms as u64));
        let outcome = {
            let watch = guard.as_mut().map_err(|e| e.clone())?;
            watch.next(timeout)
        };
        match outcome {
            Ok(LibWatchOutcome::Value(value)) => Ok(WatchOutcome::Event(
                value.unwrap_or(serde_json::Value::Null),
            )),
            Ok(LibWatchOutcome::Timeout) => Ok(WatchOutcome::Timeout),
            Ok(LibWatchOutcome::Eof) => {
                handle.ended.store(true, Ordering::SeqCst);
                Ok(WatchOutcome::Eof)
            }
            Ok(LibWatchOutcome::Cancelled) => Ok(WatchOutcome::Cancelled),
            Err(e) => {
                // The failure ended the watch: replacing the handle's
                // `ResolvedWatch` with it is what makes every later call
                // report it again instead of polling on.
                let err = watch_error(e);
                *guard = Err(err.clone());
                Err(err)
            }
        }
    })
}

/// Cancels a pending or future [`bc_watch_next`] call on `w`. Null-safe.
/// The sole function in this crate documented as safe to call from a
/// different thread than the one driving `w`'s other operations — it never
/// takes `w`'s internal lock, only sets an atomic flag a pending
/// `bc_watch_next` polls for. A `bc_watch_next` call already in flight
/// observes it within about one of [`ResolvedWatch::next`]'s poll ticks per
/// open subscription; every call after this one observes it immediately.
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
