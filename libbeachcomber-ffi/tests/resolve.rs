//! Resolution: `bc_resolve`, `bc_eval`, over `libbeachcomber::resolve`
//! (design `2026-09-05-one-tree-resolution-design.md` §5.3; outcomes 25, 26,
//! 27). The daemon is the only source of templates — a test declares one by
//! `bc_put` to `<provider>:template`, or starts the daemon from a config
//! directory holding the managed defaults — and the client renders it with
//! the supplied `cwd` and `env_json`. Runs against a real in-process daemon.

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

/// `bc_put(key, data)` at `path` (NULL for the global entry), asserting it
/// was accepted.
fn put(client: *mut BcClient, key: &str, data: &str, path: Option<&str>) {
    let key = cs(key);
    let data = cs(data);
    let path = path.map(cs);
    let v = read(unsafe {
        bc_put(
            client,
            key.as_ptr(),
            data.as_ptr(),
            ptr::null(),
            path.as_ref().map_or(ptr::null(), |p| p.as_ptr()),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
}

fn resolve(client: *mut BcClient, key: &str, cwd: &str, env: Option<&str>) -> serde_json::Value {
    let key = cs(key);
    let cwd = cs(cwd);
    let env = env.map(cs);
    read(unsafe {
        bc_resolve(
            client,
            key.as_ptr(),
            cwd.as_ptr(),
            env.as_ref().map_or(ptr::null(), |e| e.as_ptr()),
        )
    })
}

fn eval(client: *mut BcClient, src: &str, cwd: &str, env: Option<&str>) -> serde_json::Value {
    let src = cs(src);
    let cwd = cs(cwd);
    let env = env.map(cs);
    read(unsafe {
        bc_eval(
            client,
            src.as_ptr(),
            cwd.as_ptr(),
            env.as_ref().map_or(ptr::null(), |e| e.as_ptr()),
        )
    })
}

fn hit(v: &serde_json::Value) -> &serde_json::Value {
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    &v["data"]
}

fn error_kind(v: &serde_json::Value) -> &str {
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    v["error"]["kind"].as_str().unwrap_or_default()
}

// ── managed defaults (outcome 24) ────────────────────────────────────────────

/// A daemon started from a config directory holding the managed defaults
/// serves `python:template.version`, and `bc_resolve("python.version")`
/// renders it with the caller's environment — the same value `comb get
/// python.version` prints, from the same template.
#[test]
fn bc_resolve_renders_a_managed_default_template() {
    let config_dir = tempfile::tempdir().expect("tempdir");
    comb_daemon::daemon::defaults::write_managed_defaults(
        &config_dir.path().join("defaults"),
        libbeachcomber::VERSION,
    )
    .expect("write managed defaults");
    let daemon = DaemonGuard::spawn_with_config_dir(config_dir.path());
    let client = client_for(&daemon.path);
    let cwd = tempfile::tempdir().expect("tempdir");

    let v = resolve(
        client,
        "python.version",
        cwd.path().to_str().unwrap(),
        Some(r#"{"PYENV_VERSION":"3.12"}"#),
    );
    assert_eq!(hit(&v), &serde_json::json!("3.12"));

    unsafe { bc_client_free(client) };
}

// ── a template the daemon holds (outcomes 25, 27) ────────────────────────────

/// A template `put` to `scanline:template` is rendered at every depth the
/// address can name: the leaf, the group (shape kept), and the bare provider
/// — the whole namespace, rendered tree merged over the cached data, as
/// `comb get scanline -f json`. An address nothing is stored at is a miss,
/// `data: null`.
#[test]
fn bc_resolve_leaf_group_and_whole_namespace_from_a_put_template() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let cwd = "/tmp";
    let env = Some(r#"{"SCANLINE_FOO":"hi"}"#);

    put(
        client,
        "scanline:template",
        r#"{"widgets":{"foo":"{{ env.SCANLINE_FOO }}"}}"#,
        None,
    );
    put(client, "scanline", r#"{"lit":1}"#, None);

    let v = resolve(client, "scanline.widgets.foo", cwd, env);
    assert_eq!(hit(&v), &serde_json::json!("hi"));

    let v = resolve(client, "scanline.widgets", cwd, env);
    assert_eq!(hit(&v), &serde_json::json!({"foo": "hi"}));

    let v = resolve(client, "scanline", cwd, env);
    assert_eq!(
        hit(&v),
        &serde_json::json!({"lit": 1, "widgets": {"foo": "hi"}})
    );

    let v = resolve(client, "scanline.nope", cwd, env);
    assert_eq!(hit(&v), &serde_json::Value::Null);

    unsafe { bc_client_free(client) };
}

/// A template whose expression references a `cache.*` value resolves to
/// that value. `otherprov` is `put` globally (no `path`) while the resolve
/// runs at a `cwd`: a static record is read up the directory chain, and the
/// global entry is the last link of every chain (canon `field_resolution.md`
/// §"Path resolution").
#[test]
fn bc_resolve_cache_ref_template() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);

    put(client, "otherprov", r#"{"otherfield":"hello-value"}"#, None);
    put(
        client,
        "myprov:template",
        r#"{"myfield":"{{ cache.otherprov.otherfield }}"}"#,
        None,
    );

    let v = resolve(client, "myprov.myfield", "/tmp", None);
    assert_eq!(hit(&v), &serde_json::json!("hello-value"));

    unsafe { bc_client_free(client) };
}

/// A cascade `env.X or cache.p.f` takes the env term when present and falls
/// through when absent; a total miss yields `""`, not an error and not a
/// miss (canon invariant 9).
#[test]
fn bc_resolve_env_or_cache_cascade_falls_through_to_empty_string_on_miss() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    put(
        client,
        "myapp:template",
        r#"{"workspace":"{{ env.MYAPP_ENV or cache.myappcache.workspace }}"}"#,
        None,
    );

    let v = resolve(
        client,
        "myapp.workspace",
        "/tmp",
        Some(r#"{"MYAPP_ENV":"prod-env"}"#),
    );
    assert_eq!(hit(&v), &serde_json::json!("prod-env"));

    let v = resolve(client, "myapp.workspace", "/tmp", None);
    assert_eq!(hit(&v), &serde_json::json!(""));

    unsafe { bc_client_free(client) };
}

/// A template naming another provider's templated field follows it: `a.x`
/// references `b.y`, itself a template over `cache.d.w`. `cache.c.z` names no
/// registered provider — a missing ref, falsy — and is never reached once
/// `b.y` is truthy. Both `bc_eval` of `{{ a.x }}` and `bc_resolve` of `a.x`
/// reach `deep-value`.
#[test]
fn nested_template_dependency_is_followed() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);

    put(client, "d", r#"{"w":"deep-value"}"#, None);
    put(
        client,
        "a:template",
        r#"{"x":"{{ b.y or cache.c.z }}"}"#,
        None,
    );
    put(client, "b:template", r#"{"y":"{{ cache.d.w }}"}"#, None);

    let v = eval(client, "{{ a.x }}", "/tmp", None);
    assert_eq!(hit(&v), &serde_json::json!("deep-value"));

    let v = resolve(client, "a.x", "/tmp", None);
    assert_eq!(hit(&v), &serde_json::json!("deep-value"));

    unsafe { bc_client_free(client) };
}

/// `cwd` is threaded into every fetch, not merely required: a value `put` at
/// an explicit `path` is seen only when `cwd` is that path — through a raw
/// `bc_eval` ref and through a template's `cache.*` ref alike.
#[test]
fn path_scoped_ref_resolves_at_cwd() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let dir_a = tempfile::tempdir().expect("tempdir");
    let dir_b = tempfile::tempdir().expect("tempdir");
    let a = dir_a.path().to_str().unwrap();
    let b = dir_b.path().to_str().unwrap();

    put(client, "pp", r#"{"f":"here"}"#, Some(a));
    put(
        client,
        "myprov:template",
        r#"{"myfield":"{{ cache.pp.f }}"}"#,
        None,
    );

    assert_eq!(
        hit(&eval(client, "{{ pp.f }}", a, None)),
        &serde_json::json!("here")
    );
    assert_eq!(
        hit(&eval(client, "{{ pp.f }}", b, None)),
        &serde_json::json!("")
    );
    assert_eq!(
        hit(&resolve(client, "myprov.myfield", a, None)),
        &serde_json::json!("here")
    );
    assert_eq!(
        hit(&resolve(client, "myprov.myfield", b, None)),
        &serde_json::json!("")
    );

    unsafe { bc_client_free(client) };
}

// ── bc_eval forms and filters ────────────────────────────────────────────────

/// One expression syntax (canon `field_resolution.md` invariant 14): a
/// single `{{ expr }}` tag keeps the expression's natural type — a JSON
/// bool here, not the string `"true"` — and literal text around a tag is
/// string-valued.
#[test]
fn bc_eval_single_tag_is_typed_and_a_template_is_a_string() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);

    let v = eval(client, r#"{{ env.T != "" }}"#, "/tmp", Some(r#"{"T":"x"}"#));
    assert_eq!(hit(&v), &serde_json::json!(true));

    let v = eval(client, "{{ 1 + 1 }} apples", "/tmp", None);
    assert_eq!(hit(&v), &serde_json::json!("2 apples"));

    unsafe { bc_client_free(client) };
}

/// The `truncate` and `basename` filters are registered on the environment
/// `bc_eval` renders with.
#[test]
fn bc_eval_truncate_and_basename_filters() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let env = Some(r#"{"LONGVAR":"abcdefghij","PYVAR":"/foo/bar/baz"}"#);

    let v = eval(client, "{{ env.LONGVAR | truncate(5) }}", "/tmp", env);
    assert_eq!(hit(&v), &serde_json::json!("abcde..."));

    let v = eval(client, "{{ env.PYVAR | basename }}", "/tmp", env);
    assert_eq!(hit(&v), &serde_json::json!("baz"));

    unsafe { bc_client_free(client) };
}

/// A ref the daemon rejects — an unregistered provider, as a plain ref or a
/// `cache.*` ref — is a missing ref inside a template, falsy at any depth
/// (canon), so `or` and `default` both fire and nothing errors.
#[test]
fn bc_eval_rejected_ref_is_falsy() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);

    let v = eval(client, r#"{{ nope.field or "x" }}"#, "/tmp", None);
    assert_eq!(hit(&v), &serde_json::json!("x"));

    let v = eval(client, r#"{{ nope.field | default("x") }}"#, "/tmp", None);
    assert_eq!(hit(&v), &serde_json::json!("x"));

    let v = eval(client, "{{ cache.nope.x or 'fb' }}", "/tmp", None);
    assert_eq!(hit(&v), &serde_json::json!("fb"));

    unsafe { bc_client_free(client) };
}

// ── error kinds, from the resolver's typed errors ────────────────────────────

/// A source that does not compile is the caller's own input being malformed:
/// `ResolveError::Config` → `parse_error`. Both producers: a single tag whose
/// expression fails to parse, and a template with an unterminated marker.
#[test]
fn bc_eval_compile_error_is_parse_error() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);

    assert_eq!(
        error_kind(&eval(client, "{{ 1 + }}", "/tmp", None)),
        "parse_error"
    );
    assert_eq!(
        error_kind(&eval(client, "a {{ b", "/tmp", None)),
        "parse_error"
    );

    unsafe { bc_client_free(client) };
}

/// The key the caller asked for being refused by the daemon is an error in
/// the daemon's own words — `server_error` carrying its message — not a miss
/// and not a transport failure. Only a ref *inside* a template naming such a
/// key is falsy (`bc_eval_rejected_ref_is_falsy`).
#[test]
fn bc_resolve_rejected_key_is_a_server_error_in_the_daemons_words() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);

    let v = resolve(client, "a:b:c.x", "/tmp", None);
    assert_eq!(error_kind(&v), "server_error");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("invalid facet key"),
        "{v}"
    );

    unsafe { bc_client_free(client) };
}

/// The daemon itself being unreachable is a transport failure and keeps the
/// kind `CombError` gave it — never `server_error`, which is the daemon
/// answering. Both entry points.
#[test]
fn transport_failure_keeps_its_kind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("nothing-listens-here.sock");
    let client = client_for(&sock);

    for v in [
        eval(client, "{{ git.branch }}", "/tmp", None),
        resolve(client, "git.branch", "/tmp", None),
    ] {
        let kind = error_kind(&v);
        assert!(
            kind == "daemon_not_running" || kind == "connection_failed",
            "expected a transport-failure kind, got: {v}"
        );
    }

    unsafe { bc_client_free(client) };
}

/// A chain of distinct templated fields recurses once per link, and the
/// cycle guard does not bound it — no field repeats. Before the depth limit
/// a ~2000-link chain ran the native stack out inside the cdylib, which is a
/// SIGSEGV in the *host* process: not an error the caller can catch, and not
/// something `call_ffi`'s panic guard sees. The limit makes this an envelope:
/// a `Config` failure naming the field, so `parse_error`.
#[test]
fn bc_eval_deeply_nested_templates_error_rather_than_crash() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);

    let mut leaves: Vec<String> = (0..1999)
        .map(|i| format!(r#""f{i}":"{{{{ chain.f{} }}}}""#, i + 1))
        .collect();
    leaves.push(r#""f1999":"{{ cache.leaf.v }}""#.to_string());
    put(
        client,
        "chain:template",
        &format!("{{{}}}", leaves.join(",")),
        None,
    );

    let v = eval(client, "{{ chain.f0 }}", "/tmp", None);
    assert_eq!(error_kind(&v), "parse_error");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("nesting too deep"),
        "{v}"
    );

    unsafe { bc_client_free(client) };
}

// ── cwd is required ──────────────────────────────────────────────────────────

/// `cwd` is required: NULL must return an error envelope, never fall back
/// to the process's own working directory.
#[test]
fn null_cwd_is_an_error_envelope() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("terraform.workspace");
    let expr = cs("{{ env.LONGVAR | truncate(5) }}");

    let v = read(unsafe { bc_resolve(client, key.as_ptr(), ptr::null(), ptr::null()) });
    assert_eq!(v["ok"], serde_json::json!(false));

    let v = read(unsafe { bc_eval(client, expr.as_ptr(), ptr::null(), ptr::null()) });
    assert_eq!(v["ok"], serde_json::json!(false));

    unsafe { bc_client_free(client) };
}
