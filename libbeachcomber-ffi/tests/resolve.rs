//! Resolution: `bc_resolve`, `bc_eval`. Task 3.5 of
//! docs/superpowers/plans/2026-08-15-client-abi-and-sdk-refactor.md.
//!
//! Mirrors all four of Task 1.6's properties
//! (libbeachcomber/tests/resolution.rs) through the ABI: cache-ref virtual
//! field, env-or-cache cascade including miss -> "", a path expression over
//! a supplied cwd, and the truncate/basename filters. Runs against a real
//! in-process daemon so cache-ref virtual fields have something to fetch.

mod common;
use common::daemon::DaemonGuard;

use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::ptr;

use beachcomber::envelope::bc_string_free;
use beachcomber::{BcClient, bc_client_free, bc_client_new, bc_eval, bc_put, bc_resolve};

fn client_for(sock: &Path) -> *mut BcClient {
    let json = format!(
        r#"{{"socket_path":"{}","autostart":false,"timeout_ms":2000}}"#,
        sock.display()
    );
    let cs = CString::new(json).unwrap();
    unsafe { bc_client_new(cs.as_ptr()) }
}

fn cs(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn read(ptr: *mut c_char) -> serde_json::Value {
    assert!(!ptr.is_null(), "call_ffi must never return NULL");
    let body = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_string();
    unsafe { bc_string_free(ptr) };
    serde_json::from_str(&body).expect("envelope must be valid JSON")
}

/// Case 1: a virtual field whose expression references a `cache.*` value
/// resolves to that value. `otherprov` is `put` globally (no `path`) while
/// the resolve runs at a `cwd`: a virtual provider declares no path
/// expression, so the daemon's path-scoped read falls back to the global slot
/// (canon `field_resolution.md` §"Path resolution" — the prose on virtual
/// providers; invariant 2 is only about an empty/falsy path expression).
/// `bc_eval_path_scoped_ref_resolves_at_cwd` covers the other
/// half — a `put` at one directory is not visible at another.
#[test]
fn resolve_cache_ref_virtual_field() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let cwd = cs("/tmp");

    let provider_key = cs("otherprov");
    let data = cs(r#"{"otherfield":"hello-value"}"#);
    let put_v = read(unsafe {
        bc_put(
            client,
            provider_key.as_ptr(),
            data.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(put_v["ok"], serde_json::json!(true));

    let key = cs("myprov.myfield");
    let overrides = cs(r#"{"myprov.myfield":"cache.otherprov.otherfield"}"#);
    let v = read(unsafe {
        bc_resolve(
            client,
            key.as_ptr(),
            cwd.as_ptr(),
            ptr::null(),
            overrides.as_ptr(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("hello-value"));

    unsafe { bc_client_free(client) };
}

/// Case 2: a cascade `env.X or cache.p.f` takes the env term when present
/// and falls through when absent; a total miss yields `""`, not an error.
#[test]
fn resolve_env_or_cache_cascade_falls_through_to_empty_string_on_miss() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("terraform.workspace"); // built-in: "env.TF_WORKSPACE or cache.terraform.workspace"
    let cwd = cs("/tmp");

    // (a) env present -> env term wins, no daemon fetch needed.
    let env_present = cs(r#"{"TF_WORKSPACE":"prod-env"}"#);
    let v = read(unsafe {
        bc_resolve(
            client,
            key.as_ptr(),
            cwd.as_ptr(),
            env_present.as_ptr(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("prod-env"));

    // (b) both absent -> a miss yields "" rather than an error.
    let v =
        read(unsafe { bc_resolve(client, key.as_ptr(), cwd.as_ptr(), ptr::null(), ptr::null()) });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!(""));

    unsafe { bc_client_free(client) };
}

/// Case 3: a path expression evaluated over a supplied `cwd` selects the
/// expected cache coordinate.
#[test]
fn resolve_path_expression_over_supplied_cwd() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("myproject");
    let overrides =
        cs(r#"{"myproject":"'workspace-a' if cwd == '/Users/x/repo-a' else 'workspace-b'"}"#);

    let cwd_a = cs("/Users/x/repo-a");
    let v = read(unsafe {
        bc_resolve(
            client,
            key.as_ptr(),
            cwd_a.as_ptr(),
            ptr::null(),
            overrides.as_ptr(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("workspace-a"));

    let cwd_b = cs("/Users/x/repo-b");
    let v = read(unsafe {
        bc_resolve(
            client,
            key.as_ptr(),
            cwd_b.as_ptr(),
            ptr::null(),
            overrides.as_ptr(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("workspace-b"));

    unsafe { bc_client_free(client) };
}

/// Case 4: an expression using the `truncate` and `basename` filters
/// resolves through `bc_eval` — proving the filters survived the move into
/// the client crate, not just the symbols.
#[test]
fn eval_truncate_and_basename_filters() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let cwd = cs("/tmp");
    let env = cs(r#"{"LONGVAR":"abcdefghij","PYVAR":"/foo/bar/baz"}"#);

    let truncate_expr = cs("env.LONGVAR | truncate(5)");
    let v = read(unsafe {
        bc_eval(
            client,
            truncate_expr.as_ptr(),
            cwd.as_ptr(),
            env.as_ptr(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("abcde..."));

    let basename_expr = cs("env.PYVAR | basename");
    let v = read(unsafe {
        bc_eval(
            client,
            basename_expr.as_ptr(),
            cwd.as_ptr(),
            env.as_ptr(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("baz"));

    unsafe { bc_client_free(client) };
}

/// `cwd` is required: NULL must return an error envelope, never fall back
/// to the process's own working directory.
#[test]
fn resolve_null_cwd_is_an_error_envelope() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("terraform.workspace");

    let v =
        read(unsafe { bc_resolve(client, key.as_ptr(), ptr::null(), ptr::null(), ptr::null()) });
    assert_eq!(v["ok"], serde_json::json!(false));

    unsafe { bc_client_free(client) };
}

#[test]
fn eval_null_cwd_is_an_error_envelope() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let expr = cs("env.LONGVAR | truncate(5)");

    let v = read(unsafe { bc_eval(client, expr.as_ptr(), ptr::null(), ptr::null(), ptr::null()) });
    assert_eq!(v["ok"], serde_json::json!(false));

    unsafe { bc_client_free(client) };
}

/// One expression syntax (canon `field_resolution.md` invariant 14): a
/// single `{{ expr }}` tag keeps the expression's natural type — a JSON
/// bool here, not the string `"true"`.
#[test]
fn bc_eval_single_tag_typed() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let cwd = cs("/tmp");
    let env = cs(r#"{"T":"x"}"#);
    let expr = cs(r#"{{ env.T != "" }}"#);

    let v = read(unsafe {
        bc_eval(
            client,
            expr.as_ptr(),
            cwd.as_ptr(),
            env.as_ptr(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!(true));

    unsafe { bc_client_free(client) };
}

/// Literal text around a tag is string-valued.
#[test]
fn bc_eval_template_string() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let cwd = cs("/tmp");
    let expr = cs("{{ 1 + 1 }} apples");

    let v = read(unsafe {
        bc_eval(
            client,
            expr.as_ptr(),
            cwd.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("2 apples"));

    unsafe { bc_client_free(client) };
}

/// A bare expression (no tags) is still accepted and equivalent to the
/// single-tag form.
#[test]
fn bc_eval_bare_still_works() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let cwd = cs("/tmp");
    let env = cs(r#"{"T":"x"}"#);
    let expr = cs("env.T");

    let v = read(unsafe {
        bc_eval(
            client,
            expr.as_ptr(),
            cwd.as_ptr(),
            env.as_ptr(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("x"));

    unsafe { bc_client_free(client) };
}

/// What `eval::daemon_refs`'s transitive closure over virtual fields buys
/// over the old one-level `fetch_cache_refs`: `a.x` references virtual
/// `b.y`, which itself references `cache.d.w` — a daemon key two hops away
/// from the field being evaluated. It must still be fetched and threaded
/// through before evaluation, or `b.y` (and so `a.x`) is falsy.
///
/// `c` is never registered, so `cache.c.z` in `a.x`'s `or` fallback hits
/// the daemon's "unknown provider" rejection — a missing ref, not an
/// error (see `bc_eval_unknown_provider_is_a_miss`) — and is never reached
/// anyway once `b.y` resolves truthy. `d` is `put` globally and read at a
/// `cwd`: a virtual provider falls back to the global slot (canon
/// `field_resolution.md` §"Path resolution" prose, not invariant 2).
#[test]
fn bc_eval_nested_virtual_dependency_is_fetched() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let cwd = cs("/tmp");

    let provider_key = cs("d");
    let data = cs(r#"{"w":"deep-value"}"#);
    let put_v = read(unsafe {
        bc_put(
            client,
            provider_key.as_ptr(),
            data.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(put_v["ok"], serde_json::json!(true), "{put_v}");

    let overrides = cs(r#"{"a.x":"{{ b.y or cache.c.z }}","b.y":"{{ cache.d.w }}"}"#);
    let expr = cs("a.x");
    let v = read(unsafe {
        bc_eval(
            client,
            expr.as_ptr(),
            cwd.as_ptr(),
            ptr::null(),
            overrides.as_ptr(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("deep-value"));

    unsafe { bc_client_free(client) };
}

/// `bc_resolve` twin of `bc_eval_nested_virtual_dependency_is_fetched`:
/// `eval::daemon_refs`'s transitive closure over virtual fields applies to
/// a declared field's own expression too, not just a raw `bc_eval` source.
/// `d` is `put` globally and read at a `cwd`: a virtual provider falls back
/// to the global slot (canon `field_resolution.md` §"Path resolution" prose,
/// not invariant 2).
#[test]
fn bc_resolve_nested_virtual_dependency_is_fetched() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let cwd = cs("/tmp");

    let provider_key = cs("d");
    let data = cs(r#"{"w":"deep-value"}"#);
    let put_v = read(unsafe {
        bc_put(
            client,
            provider_key.as_ptr(),
            data.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(put_v["ok"], serde_json::json!(true), "{put_v}");

    let overrides = cs(r#"{"a.x":"{{ b.y or cache.c.z }}","b.y":"{{ cache.d.w }}"}"#);
    let key = cs("a.x");
    let v = read(unsafe {
        bc_resolve(
            client,
            key.as_ptr(),
            cwd.as_ptr(),
            ptr::null(),
            overrides.as_ptr(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("deep-value"));

    unsafe { bc_client_free(client) };
}

/// A ref naming a provider the daemon has never heard of (`"unknown
/// provider: nope"`, a `CombError::ServerError`) is a missing ref, not a
/// transport failure — canon says a missing ref is falsy at any depth, and
/// this is the same rule `src/client.rs`'s `to_response` applies for the
/// CLI (`comb eval '{{ c.z or "x" }}'` prints `x`, never errors).
#[test]
fn bc_eval_unknown_provider_is_a_miss() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let cwd = cs("/tmp");

    let or_expr = cs(r#"{{ nope.field or "x" }}"#);
    let v = read(unsafe {
        bc_eval(
            client,
            or_expr.as_ptr(),
            cwd.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("x"));

    let default_expr = cs(r#"{{ nope.field | default("x") }}"#);
    let v = read(unsafe {
        bc_eval(
            client,
            default_expr.as_ptr(),
            cwd.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("x"));

    unsafe { bc_client_free(client) };
}

/// A compile failure in the expression itself is the caller's own input
/// being malformed — `parse_error`, matching the kind `bc_resolve` uses for
/// the same failure in a declared virtual field's expression. Covers both
/// compile-error producers: an expression that fails to parse, and a
/// template with an unterminated tag marker (`eval::render_template`'s
/// "template compile error: " path, not `eval::evaluate`'s typed one).
#[test]
fn bc_eval_compile_error_is_parse_error_kind() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let cwd = cs("/tmp");
    let expr = cs("{{ 1 +++ }}");

    let v = read(unsafe {
        bc_eval(
            client,
            expr.as_ptr(),
            cwd.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    assert_eq!(v["error"]["kind"], serde_json::json!("parse_error"), "{v}");

    let unterminated = cs("a {{ b");
    let v = read(unsafe {
        bc_eval(
            client,
            unterminated.as_ptr(),
            cwd.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    assert_eq!(v["error"]["kind"], serde_json::json!("parse_error"), "{v}");

    unsafe { bc_client_free(client) };
}

/// `bc_resolve` twin of `bc_eval_compile_error_is_parse_error_kind`. Also
/// pins that `eval_error_kind` sees through the `"provider.field: "` prefix
/// `VirtualFields::evaluate` wraps the underlying "... compile error: ..."
/// message in — the substring check still has to fire after that wrapping.
#[test]
fn bc_resolve_compile_error_is_parse_error_kind() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let cwd = cs("/tmp");
    let key = cs("a.x");
    let overrides = cs(r#"{"a.x":"{{ 1 +++ }}"}"#);

    let v = read(unsafe {
        bc_resolve(
            client,
            key.as_ptr(),
            cwd.as_ptr(),
            ptr::null(),
            overrides.as_ptr(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    assert_eq!(v["error"]["kind"], serde_json::json!("parse_error"), "{v}");

    unsafe { bc_client_free(client) };
}

/// The negative half of `bc_eval_compile_error_is_parse_error_kind`: a source
/// that compiles fine and then fails at *evaluation* is a `server_error`, not
/// a `parse_error`. Without this, `eval_error_kind`'s `"compile error"`
/// substring check could be widened (or the prefixes reworded) and nothing
/// would notice that every runtime failure had started reporting as the
/// caller's syntax being wrong.
///
/// Covers both runtime producers: `eval::evaluate`'s typed path
/// ("expression eval error: ", from a single tag whose operands are
/// incompatible) and `eval::render_template`'s ("template render error: ",
/// from a `{% for %}` over a non-iterable — a template, so the render path).
#[test]
fn bc_eval_runtime_error_is_server_error_kind() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let cwd = cs("/tmp");

    let bad_operands = cs(r#"{{ 1 / "a" }}"#);
    let v = read(unsafe {
        bc_eval(
            client,
            bad_operands.as_ptr(),
            cwd.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    assert_eq!(v["error"]["kind"], serde_json::json!("server_error"), "{v}");

    let bad_loop = cs("{% for x in 5 %}{{ x }}{% endfor %}");
    let v = read(unsafe {
        bc_eval(
            client,
            bad_loop.as_ptr(),
            cwd.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    assert_eq!(v["error"]["kind"], serde_json::json!("server_error"), "{v}");

    unsafe { bc_client_free(client) };
}

/// `bc_resolve` twin of `bc_eval_runtime_error_is_server_error_kind`: a
/// declared virtual field that compiles and then fails at evaluation is a
/// `server_error` too, and the `"provider.field: "` prefix
/// `VirtualFields::evaluate` adds must not push it over into `parse_error`.
#[test]
fn bc_resolve_runtime_error_is_server_error_kind() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let cwd = cs("/tmp");
    let key = cs("a.x");
    let overrides = cs(r#"{"a.x":"{{ 1 / \"a\" }}"}"#);

    let v = read(unsafe {
        bc_resolve(
            client,
            key.as_ptr(),
            cwd.as_ptr(),
            ptr::null(),
            overrides.as_ptr(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    assert_eq!(v["error"]["kind"], serde_json::json!("server_error"), "{v}");

    unsafe { bc_client_free(client) };
}

/// A ref naming a provider the daemon has never heard of is a missing ref
/// (`bc_eval_unknown_provider_is_a_miss`), but the daemon itself being
/// unreachable is a genuine transport failure and aborts the whole fetch
/// with an error envelope rather than silently evaluating against an empty
/// map.
#[test]
fn bc_eval_transport_failure_aborts() {
    let sock = Path::new("/tmp/beachcomber-ffi-resolve-test-unreachable.sock");
    let _ = std::fs::remove_file(sock);
    let client = client_for(sock);
    let cwd = cs("/tmp");
    let expr = cs("{{ git.branch }}");

    let v = read(unsafe {
        bc_eval(
            client,
            expr.as_ptr(),
            cwd.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    let kind = v["error"]["kind"].as_str().unwrap_or("");
    assert!(
        kind == "daemon_not_running" || kind == "connection_failed",
        "expected a transport-failure kind, got: {v}"
    );

    unsafe { bc_client_free(client) };
}

/// `cwd` is threaded into the daemon fetch, not merely required and
/// ignored: a path-scoped provider — `put` at an explicit `path` — resolves
/// only when `bc_eval`'s `cwd` matches that path, the same convention
/// `comb get`'s `--path` / `comb eval`'s `set_context` follow.
#[test]
fn bc_eval_path_scoped_ref_resolves_at_cwd() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let dir_a = tempfile::tempdir().expect("tempdir");
    let dir_b = tempfile::tempdir().expect("tempdir");
    let dir_a_path = cs(dir_a.path().to_str().unwrap());
    let dir_b_path = cs(dir_b.path().to_str().unwrap());

    let key = cs("pp");
    let data = cs(r#"{"f":"here"}"#);
    let put_v = read(unsafe {
        bc_put(
            client,
            key.as_ptr(),
            data.as_ptr(),
            ptr::null(),
            dir_a_path.as_ptr(),
        )
    });
    assert_eq!(put_v["ok"], serde_json::json!(true), "{put_v}");

    let expr = cs("{{ pp.f }}");
    let v = read(unsafe {
        bc_eval(
            client,
            expr.as_ptr(),
            dir_a_path.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("here"));

    let v = read(unsafe {
        bc_eval(
            client,
            expr.as_ptr(),
            dir_b_path.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!(""));

    unsafe { bc_client_free(client) };
}

/// `bc_resolve` twin of `bc_eval_path_scoped_ref_resolves_at_cwd`: a
/// declared field's `cache.*` ref is fetched at the resolver's own `cwd`.
#[test]
fn bc_resolve_path_scoped_ref_resolves_at_cwd() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let dir_a = tempfile::tempdir().expect("tempdir");
    let dir_b = tempfile::tempdir().expect("tempdir");
    let dir_a_path = cs(dir_a.path().to_str().unwrap());
    let dir_b_path = cs(dir_b.path().to_str().unwrap());

    let key = cs("pp");
    let data = cs(r#"{"f":"here"}"#);
    let put_v = read(unsafe {
        bc_put(
            client,
            key.as_ptr(),
            data.as_ptr(),
            ptr::null(),
            dir_a_path.as_ptr(),
        )
    });
    assert_eq!(put_v["ok"], serde_json::json!(true), "{put_v}");

    let resolve_key = cs("myprov.myfield");
    let overrides = cs(r#"{"myprov.myfield":"cache.pp.f"}"#);
    let v = read(unsafe {
        bc_resolve(
            client,
            resolve_key.as_ptr(),
            dir_a_path.as_ptr(),
            ptr::null(),
            overrides.as_ptr(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("here"));

    let v = read(unsafe {
        bc_resolve(
            client,
            resolve_key.as_ptr(),
            dir_b_path.as_ptr(),
            ptr::null(),
            overrides.as_ptr(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!(""));

    unsafe { bc_client_free(client) };
}
