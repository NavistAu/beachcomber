//! C ABI surface for `libbeachcomber`. This crate contains no logic of its
//! own — every `extern "C"` entry point is a thin wrapper delegating to
//! `libbeachcomber`.

use std::ffi::{CStr, CString, c_char};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub mod envelope;

use envelope::{ErrorKind, FfiError, WatchOutcome, call_ffi, call_watch_next};
use libbeachcomber::eval;
use libbeachcomber::path_expr::{evaluate_path, path_expression_for};
use libbeachcomber::virtual_fields::{EvalContext, Ref, VirtualFields};
use libbeachcomber::{
    CacheRow, CombError, CombResult, DaemonHealth, IntrospectResponse, IntrospectSubject,
};
use std::collections::{HashMap, HashSet};

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

/// Field-expression overrides, keyed `(provider, field)`.
type FieldOverrides = Vec<((String, String), String)>;

/// Parses a nullable `overrides_json` argument into field-expression
/// overrides (keyed `"provider.field"`) and path-expression overrides
/// (keyed by a bare provider name) — the same split the conformance
/// runner's fixture `virtual` block uses. `None` (NULL) means "no
/// overrides" — [`VirtualFields::with_config_overrides`] still applies the
/// built-in defaults underneath an empty override list.
fn parse_overrides_json(
    json: Option<&str>,
) -> Result<(FieldOverrides, HashMap<String, String>), FfiError> {
    let Some(s) = json else {
        return Ok((Vec::new(), HashMap::new()));
    };
    let v: serde_json::Value = serde_json::from_str(s).map_err(|e| {
        FfiError::new(
            ErrorKind::ParseError,
            format!("malformed overrides_json: {e}"),
        )
    })?;
    let obj = v.as_object().ok_or_else(|| {
        FfiError::new(
            ErrorKind::ParseError,
            "overrides_json must be a JSON object",
        )
    })?;
    let mut field_overrides = Vec::new();
    let mut path_overrides = HashMap::new();
    for (key, expr) in obj {
        let expr = expr.as_str().ok_or_else(|| {
            FfiError::new(
                ErrorKind::ParseError,
                format!("overrides_json[{key}] must be a string expression"),
            )
        })?;
        match key.split_once('.') {
            Some((provider, field)) => {
                field_overrides.push(((provider.to_string(), field.to_string()), expr.to_string()));
            }
            None => {
                path_overrides.insert(key.clone(), expr.to_string());
            }
        }
    }
    Ok((field_overrides, path_overrides))
}

/// Fetches every ref in `refs` from the daemon via `client.get(key,
/// Some(cwd))`, into a daemon-data map an [`EvalContext`] can borrow. `cwd`
/// is threaded through so path-scoped providers (`git`, `terraform`,
/// `python`, `kubecontext`, …) resolve at the caller's supplied directory —
/// the same convention `comb get`'s `--path` and `comb eval`'s
/// `set_context` follow. Keying and dedup are [`eval::fetch_daemon_data`]'s;
/// this closure only translates one `client.get` call into the
/// `Result<Option<Value>, FfiError>` shape it wants. A cache miss is simply
/// absent from the map and evaluates falsy.
///
/// So is `CombError::ServerError` — the daemon answered, but rejected the
/// key (e.g. "unknown provider: c" for a `cache.*` ref that names nothing
/// registered). Canon: a missing ref is falsy at any depth, and a rejected
/// key is exactly that, not a transport failure. This mirrors
/// `src/client.rs`'s `to_response`, which turns the very same
/// `CombError::ServerError` into a response with `data: None` for the CLI
/// (`comb eval '{{ c.z or "x" }}'` prints `x`) — so the FFI and the CLI
/// agree on what a rejected key means. Every other `CombError` variant
/// (connection/IO/parse/timeout/daemon-not-running) is a genuine transport
/// failure and aborts the whole fetch: the [`CombError`]-derived envelope
/// error every other `bc_*` op already produces via `?`, with the failing
/// key named in the message the way `run_eval`'s own fetch closure names it.
fn fetch_via_client(
    client: &libbeachcomber::Client,
    cwd: &str,
    refs: &[Ref],
) -> Result<HashMap<String, serde_json::Value>, FfiError> {
    eval::fetch_daemon_data(refs, |key| match client.get(key, Some(cwd)) {
        Ok(CombResult::Hit { data, .. }) => Ok(Some(data.as_value().clone())),
        Ok(CombResult::Miss) => Ok(None),
        Err(CombError::ServerError(_)) => Ok(None),
        Err(e) => {
            let mapped = FfiError::from(e);
            Err(FfiError::new(
                mapped.kind,
                format!("querying {key}: {}", mapped.message),
            ))
        }
    })
}

/// Maps an evaluation error message from [`eval::evaluate`] /
/// [`VirtualFields::evaluate`] to an [`ErrorKind`]. Both
/// `eval::evaluate`'s typed path and `eval::render_template` prefix a
/// compile failure with `"expression compile error: "` / `"template compile
/// error: "` (optionally itself wrapped by `VirtualFields::evaluate`'s
/// `"provider.field: "`) — that is the caller's own expression being
/// malformed, a `parse_error`. Everything else (a runtime eval/render
/// failure, an evaluation cycle) is a `server_error`. There is no typed
/// error to match on here — both functions return a plain `String` — so
/// this is deliberately a substring check on the one phrase both compile
/// paths share.
fn eval_error_kind(message: &str) -> ErrorKind {
    if message.contains("compile error") {
        ErrorKind::ParseError
    } else {
        ErrorKind::ServerError
    }
}

/// Resolve a virtual field (`key` = `"provider.field"`) or a path
/// expression (`key` = a bare provider name) — client-side, exactly as
/// `comb get`'s resolution layer does.
///
/// `cwd` is required: NULL returns an error envelope rather than falling
/// back to the process's own working directory — this library must never
/// read ambient state on the caller's behalf. `env_json` and
/// `overrides_json` are nullable, meaning "none supplied".
///
/// # Safety
/// `client` must be a valid pointer from [`bc_client_new`]; `key` and `cwd`
/// must be non-null and NUL-terminated; `env_json` and `overrides_json` must
/// be NULL or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_resolve(
    client: *mut BcClient,
    key: *const c_char,
    cwd: *const c_char,
    env_json: *const c_char,
    overrides_json: *const c_char,
) -> *mut c_char {
    call_ffi(move || {
        let c = unsafe { client_ref(client) }?;
        let key = unsafe { required_str(key, "key") }?;
        let cwd = unsafe { required_str(cwd, "cwd") }?;
        let env_json = unsafe { optional_str(env_json) }?;
        let overrides_json = unsafe { optional_str(overrides_json) }?;
        let env_vars = parse_env_json(env_json)?;
        let (field_overrides, path_overrides) = parse_overrides_json(overrides_json)?;
        let vf = VirtualFields::with_config_overrides(field_overrides);

        match key.split_once('.') {
            Some((provider, field)) => {
                let expr = vf
                    .expression(provider, field)
                    .ok_or_else(|| {
                        FfiError::new(
                            ErrorKind::ParseError,
                            format!("{provider}.{field} is not a virtual field"),
                        )
                    })?
                    .to_string();
                let refs = eval::daemon_refs(&expr, &vf);
                let daemon_data = fetch_via_client(c, cwd, &refs)?;
                let ctx = EvalContext {
                    env_vars: &env_vars,
                    daemon_data: &daemon_data,
                };
                let v = vf
                    .evaluate(provider, field, &ctx, &mut HashSet::new())
                    .map_err(|e| FfiError::new(eval_error_kind(&e), e))?;
                Ok(v)
            }
            None => match path_expression_for(key, &path_overrides) {
                Some(expr) => match evaluate_path(&expr, cwd, &env_vars) {
                    Some(s) => Ok(serde_json::Value::String(s)),
                    None => Ok(serde_json::Value::Null),
                },
                None => Err(FfiError::new(
                    ErrorKind::ParseError,
                    format!("{key} has no declared path expression"),
                )),
            },
        }
    })
}

/// Evaluate a value expression in any of the three forms canon
/// `field_resolution.md` (invariant 14) defines: a bare expression, exactly
/// one `{{ expr }}` tag (keeps the expression's natural type), or literal
/// text and/or several tags (string-valued) — the same evaluator
/// `bc_resolve` uses for a declared virtual field, but for a raw expression
/// that need not be registered anywhere. Every reference the source makes
/// is threaded in: `env.*` from `env_json`, and `cache.*` / plain
/// `provider.field` refs fetched from the daemon, following virtual fields
/// transitively — the same closure `bc_resolve` uses. A missing or unknown
/// ref (an absent `env.*`, a daemon miss, or a daemon-rejected key such as
/// an unregistered provider) is falsy at any depth, not an error.
///
/// `cwd` is required, matching `bc_resolve`'s signature (see there for
/// why) — a bare `cwd` reference in a field expression is reserved for a
/// later task and not evaluated by `bc_eval` today.
///
/// # Safety
/// `client` must be a valid pointer from [`bc_client_new`]; `template_str`
/// and `cwd` must be non-null and NUL-terminated; `env_json` and
/// `overrides_json` must be NULL or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_eval(
    client: *mut BcClient,
    template_str: *const c_char,
    cwd: *const c_char,
    env_json: *const c_char,
    overrides_json: *const c_char,
) -> *mut c_char {
    call_ffi(move || {
        let c = unsafe { client_ref(client) }?;
        let template_str = unsafe { required_str(template_str, "template_str") }?;
        let cwd = unsafe { required_str(cwd, "cwd") }?;
        let env_json = unsafe { optional_str(env_json) }?;
        let overrides_json = unsafe { optional_str(overrides_json) }?;
        let env_vars = parse_env_json(env_json)?;
        let (field_overrides, _path_overrides) = parse_overrides_json(overrides_json)?;
        let vf = VirtualFields::with_config_overrides(field_overrides);

        // Every daemon key this source needs, virtual-field dependencies
        // included — mirrors the CLI's `run_eval`.
        let refs = eval::daemon_refs(template_str, &vf);
        let daemon_data = fetch_via_client(c, cwd, &refs)?;
        let ctx = EvalContext {
            env_vars: &env_vars,
            daemon_data: &daemon_data,
        };
        eval::evaluate(template_str, &vf, &ctx).map_err(|e| FfiError::new(eval_error_kind(&e), e))
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

/// Query a single key on this session's persistent connection. `flags` is
/// the same [`BC_GET_FORCE`] / [`BC_GET_WAIT`] bitmask [`bc_get`] takes.
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

/// Store data into a virtual provider on this session's persistent
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
/// (a malformed response line, or the `ServerError`/`ParseError` a bad
/// nested field path produces) — any lower-level socket failure (a reset
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
