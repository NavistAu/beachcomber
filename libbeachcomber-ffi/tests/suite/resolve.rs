//! Resolution: `bc_get`, `bc_eval`, over `libbeachcomber::resolve`
//! (design `2026-09-05-one-tree-resolution-design.md` §5.3; outcomes 25, 26,
//! 27). The daemon is the only source of templates — a test declares one by
//! `bc_put` to `<provider>:template`, or starts the daemon from a config
//! directory holding the managed defaults — and the client renders it with
//! the supplied `cwd` and `env_json`. Runs against a real in-process daemon.

use crate::common::client::{Options, client_for, client_with, cs, read};
use crate::common::daemon::DaemonGuard;

use std::ptr;

use beachcomber::{BcClient, bc_client_free, bc_eval, bc_get, bc_put};

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
            false,
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
}

fn resolve(client: *mut BcClient, key: &str, cwd: &str, env: Option<&str>) -> serde_json::Value {
    let key = cs(key);
    let cwd = cs(cwd);
    let env = env.map(cs);
    read(unsafe {
        bc_get(
            client,
            key.as_ptr(),
            cwd.as_ptr(),
            env.as_ref().map_or(ptr::null(), |e| e.as_ptr()),
            ptr::null(),
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
/// serves `python:template.version`, and `bc_get("python.version")`
/// renders it with the caller's environment — the same value `comb get
/// python.version` prints, from the same template.
#[test]
fn bc_get_renders_a_managed_default_template() {
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

/// `kubecontext.context` is a managed-default template over `exec('kubectl …')`
/// (outcome 20). A client that did not opt into `exec` renders it as the
/// disabled-exec failure — a `server_error`, the resolver's runtime fault —
/// rather than spawning anything.
#[test]
fn bc_get_of_kubecontext_without_exec_is_the_disabled_error() {
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
        "kubecontext.context",
        cwd.path().to_str().unwrap(),
        None,
    );
    assert_eq!(error_kind(&v), "server_error", "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("exec() is disabled"),
        "unexpected error: {v}"
    );

    unsafe { bc_client_free(client) };
}

// ── a template the daemon holds (outcomes 25, 27) ────────────────────────────

/// A template `put` to `scanline:template` is rendered at every depth the
/// address can name: the leaf, the group (shape kept), and the bare provider
/// — the whole namespace, rendered tree merged over the cached data, as
/// `comb get scanline -f json`. An address nothing is stored at is a miss,
/// `data: null`.
#[test]
fn bc_get_leaf_group_and_whole_namespace_from_a_put_template() {
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

/// `cwd` is threaded into every fetch, not merely required: a value `put` at
/// an explicit `path` is seen only when `cwd` is that path — through a raw
/// `bc_eval` ref and through a template's `comb.*` ref alike.
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
        r#"{"myfield":"{{ comb.pp.f }}"}"#,
        None,
    );

    assert_eq!(
        hit(&eval(client, "{{ pp.f }}", a, None)),
        &serde_json::json!("here")
    );
    assert_eq!(
        hit(&eval(client, "{{ pp.f }}", b, None)),
        &serde_json::Value::Null
    );
    assert_eq!(
        hit(&resolve(client, "myprov.myfield", a, None)),
        &serde_json::json!("here")
    );
    assert_eq!(
        hit(&resolve(client, "myprov.myfield", b, None)),
        &serde_json::Value::Null
    );

    unsafe { bc_client_free(client) };
}

// ── bc_eval forms and filters ────────────────────────────────────────────────

// ── error kinds, from the resolver's typed errors ────────────────────────────

/// A source that does not compile is the caller's own input being malformed:
/// `ResolveError::Config` → `parse_error`. Both producers: a single tag whose
/// expression fails to parse, and a template with an unterminated marker.
/// `bc_eval` end to end across the C ABI, which the conformance fixtures do not
/// reach (their runner calls `resolve::eval`): `env_json` is read, a single
/// `{{ expr }}` tag keeps the expression's type, literal text around a tag is a
/// string, and a tag whose value is `none` is absent, as a miss is.
#[test]
fn bc_eval_single_tag_is_typed_and_a_template_is_a_string() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);

    let v = eval(client, r#"{{ env.T != "" }}"#, "/tmp", Some(r#"{"T":"x"}"#));
    assert_eq!(hit(&v), &serde_json::json!(true));

    let v = eval(client, "{{ 1 + 1 }} apples", "/tmp", None);
    assert_eq!(hit(&v), &serde_json::json!("2 apples"));

    let v = eval(client, "{{ none }}", "/tmp", None);
    assert_eq!(hit(&v), &serde_json::Value::Null);

    unsafe { bc_client_free(client) };
}

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
/// key is falsy (the `eval/missing_ref_is_falsy` conformance fixture).
#[test]
fn bc_get_rejected_key_is_a_server_error_in_the_daemons_words() {
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
    let client = client_with(
        &sock,
        Options {
            connect_timeout_ms: Some(0),
            ..Options::default()
        },
    );

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
/// something `call_ffi`'s panic guard sees. The limit (128 links) makes this
/// an envelope: a `Config` failure naming the field, so `parse_error`. A
/// chain of 130 crosses it; a longer one stops at the same link.
#[test]
fn bc_eval_deeply_nested_templates_error_rather_than_crash() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);

    let mut leaves: Vec<String> = (0..129)
        .map(|i| format!(r#""f{i}":"{{{{ chain.f{} }}}}""#, i + 1))
        .collect();
    leaves.push(r#""f129":"{{ comb.leaf.v }}""#.to_string());
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

// ── the directory a resolution addresses ─────────────────────────────────────

/// A NULL `cwd` addresses the global entry, as `comb get --global` does: a
/// value put there is found, and one put at a directory is not.
#[test]
fn null_cwd_addresses_the_global_entry() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    put(client, "nullcwd", r#"{"v":"global"}"#, None);
    put(client, "nullcwd", r#"{"w":"dir"}"#, Some("/tmp"));
    let key = cs("nullcwd.v");
    let src = cs("{{ nullcwd.v }}-{{ nullcwd.w or 'none' }}");

    let v = read(unsafe { bc_get(client, key.as_ptr(), ptr::null(), ptr::null(), ptr::null()) });
    assert_eq!(hit(&v), &serde_json::json!("global"), "{v}");

    let v = read(unsafe { bc_eval(client, src.as_ptr(), ptr::null(), ptr::null()) });
    assert_eq!(hit(&v), &serde_json::json!("global-none"), "{v}");

    unsafe { bc_client_free(client) };
}

/// A relative `cwd` is the caller's, made absolute against this process's
/// directory exactly as a relative `comb get --path` is: the same read as
/// naming that directory outright.
#[test]
fn a_relative_cwd_is_the_callers_directory() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let here = std::env::current_dir().unwrap();
    put(
        client,
        "relcwd",
        r#"{"v":"here"}"#,
        Some(here.to_str().unwrap()),
    );

    let v = resolve(client, "relcwd.v", ".", None);
    assert_eq!(hit(&v), &serde_json::json!("here"), "{v}");

    unsafe { bc_client_free(client) };
}

// ── a call with no env renders with the process environment ──────────────────

/// A `bc_get` that passes no `env_json` renders `env.*` from this process, as
/// `comb get` does.
#[test]
fn a_get_with_no_env_reads_the_process_home() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    put(
        client,
        "noenv:template",
        r#"{"home":"{{ env.HOME }}"}"#,
        None,
    );

    let v = resolve(client, "noenv.home", "/tmp", None);
    assert_eq!(hit(&v), &serde_json::json!(std::env::var("HOME").unwrap()));

    // An explicit map is the whole environment: HOME is absent from it.
    let v = resolve(client, "noenv.home", "/tmp", Some("{}"));
    assert_eq!(hit(&v), &serde_json::Value::Null, "{v}");

    unsafe { bc_client_free(client) };
}

/// `bc_eval` with no `env_json` is the same.
#[test]
fn an_eval_with_no_env_reads_the_process_home() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);

    let v = eval(client, "{{ env.HOME }}", "/tmp", None);
    assert_eq!(hit(&v), &serde_json::json!(std::env::var("HOME").unwrap()));

    unsafe { bc_client_free(client) };
}

/// An `exec()` child of a call with no env inherits this process's `PATH`: a
/// child's environment is exactly the render's `env`.
#[test]
fn an_exec_child_of_a_call_with_no_env_inherits_the_process_path() {
    let daemon = DaemonGuard::spawn();
    let client = client_with(
        &daemon.path,
        Options {
            exec: true,
            ..Options::default()
        },
    );

    let v = eval(client, r#"{{ exec('echo "$PATH"') | trim }}"#, "/tmp", None);
    assert_eq!(hit(&v), &serde_json::json!(std::env::var("PATH").unwrap()));

    unsafe { bc_client_free(client) };
}
