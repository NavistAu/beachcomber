//! C ABI surface for `libbeachcomber`. This crate contains no logic of its
//! own — every `extern "C"` entry point is a thin wrapper delegating to
//! `libbeachcomber`.

use std::ffi::{CStr, CString, c_char};
use std::sync::OnceLock;
use std::time::Duration;

pub mod envelope;

use envelope::{ErrorKind, FfiError, call_ffi};
use libbeachcomber::{CacheRow, CombResult, DaemonHealth, IntrospectResponse, IntrospectSubject};

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
    };

    let mut client = libbeachcomber::Client::with_config(config);
    if let Some(socket_path) = options.socket_path {
        client = client.with_socket_path(std::path::PathBuf::from(socket_path));
    }
    Ok(client)
}

/// Creates a new client handle. `options_json` is nullable; recognised keys
/// are `socket_path`, `timeout_ms` and `autostart`. Never returns NULL — a
/// malformed `options_json` still yields a handle, with the parse error
/// deferred to the handle's first operation rather than failing here.
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

fn combresult_to_json(result: CombResult) -> serde_json::Value {
    match result {
        CombResult::Hit {
            data,
            age_ms,
            stale,
        } => serde_json::json!({
            "data": data.as_value(),
            "age_ms": age_ms as u64,
            "stale": stale,
        }),
        CombResult::Miss => serde_json::json!({
            "data": null,
            "age_ms": null,
            "stale": null,
        }),
    }
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

/// Query a single key. `path` is nullable (global providers). `flags` is a
/// bitmask of [`BC_GET_FORCE`] / [`BC_GET_WAIT`]; any other bit set yields a
/// `bad_flags` envelope.
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

/// Store data into a virtual provider. `json_data` must parse as JSON.
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

/// Clear the cached entry for a virtual provider key without dropping the
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

/// Trigger recomputation of a provider. `path` is nullable.
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
