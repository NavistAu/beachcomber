mod common;
use common::daemon::TestDaemon;
use std::time::Duration;

#[test]
fn daemon_fixture_starts_and_exits() {
    let d = TestDaemon::spawn();
    assert!(d.socket.path.exists());
    drop(d);
}

// ── helper ────────────────────────────────────────────────────────────────────

/// Build a `comb` Command pre-configured with `BEACHCOMBER_SOCKET` pointing
/// directly at the test daemon's socket path, so `Config::resolve_socket_path()`
/// resolves to the isolated test socket.
///
/// The current_dir is fixed to "/" so that path-scoped virtual keys written with
/// `--path /` are found by `get` (which always injects CWD as path context for
/// virtual providers).
fn comb(d: &TestDaemon) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &d.socket.path);
    // Suppress daemon-start output from bleeding into stdout assertions.
    cmd.env("RUST_LOG", "error");
    // Fix CWD to "/" so `get` injects a stable, known path context. Virtual
    // keys must be stored with `--path /` (or `--path .` in this CWD).
    cmd.current_dir("/");
    cmd
}

// ── golden_get_returns_known_value ────────────────────────────────────────────

#[test]
fn golden_get_returns_known_value() {
    let d = TestDaemon::spawn();

    // Seed a virtual key.  `get` always injects CWD as path context for virtual
    // providers; we fix CWD to "/" (see `comb()` helper) so we must also put
    // with `--path /` to store under the same cache key.
    comb(&d)
        .args(["put", "myapp", r#"{"status":"healthy"}"#, "--path", "/"])
        .assert()
        .success();

    comb(&d)
        .args(["get", "myapp.status"])
        .assert()
        .success()
        .stdout(predicates::str::contains("healthy"));
}

// ── golden_status_shows_running_daemon ────────────────────────────────────────

#[test]
fn golden_status_shows_running_daemon() {
    let d = TestDaemon::spawn();

    // `comb status` talks to the daemon and prints cache rows or column headers.
    // With an empty cache it may print nothing, but it must exit 0.
    comb(&d).args(["status"]).assert().success();
}

// ── golden_put_sets_a_value ───────────────────────────────────────────────────

#[test]
fn golden_put_sets_a_value() {
    let d = TestDaemon::spawn();

    // put exits 0; a subsequent get returns the stored value.
    // `--path /` matches the "/" CWD injected by get's path resolution.
    comb(&d)
        .args(["put", "testprov", r#"{"field":"golden"}"#, "--path", "/"])
        .assert()
        .success();

    comb(&d)
        .args(["get", "testprov.field"])
        .assert()
        .success()
        .stdout(predicates::str::contains("golden"));
}

// ── get_json_finds_put_created_virtual_without_explicit_path ─────────────────

/// Regression test for the bug where `comb put myvirtual '{"field1":"hello"}'`
/// (no `--path`, so the entry is stored globally) followed by
/// `comb get myvirtual.field1 -f json` (also no `--path`) exited 2 with empty
/// stdout AND stderr, even though the identical request over the raw socket
/// (no path field at all) answers correctly. Root cause: `get`'s CLI layer
/// defaults the request path to the process CWD when neither `--path` nor a
/// trailing positional path is given, which scopes the lookup away from the
/// global entry `put` created. `get` must still find a globally-put virtual
/// entry when no path was explicitly requested.
#[test]
fn get_json_finds_put_created_virtual_without_explicit_path() {
    let d = TestDaemon::spawn();

    comb(&d)
        .args(["put", "myvirtual", r#"{"field1":"hello"}"#])
        .assert()
        .success();

    comb(&d)
        .args(["get", "myvirtual.field1", "-f", "json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("hello"));
}

// ── single_key_get_error_paths_always_write_stderr ────────────────────────────

/// Regression test: every path in `run_get`'s single-key client-side rendering
/// block that exits non-zero must also write something to stderr. Before the
/// fix, the `ok:true`/`data:None` (cache-miss) branch set the error flag with
/// no accompanying `eprintln!`, so the process exited 2 with zero bytes on
/// both stdout and stderr — indistinguishable from a hang or a swallowed
/// panic. This exercises a genuine miss (an unknown field on an existing
/// virtual provider, path-matched on both ends) rather than the routing bug
/// above, to isolate the "silent failure" defect from the "wrong routing"
/// defect.
#[test]
fn single_key_get_error_paths_always_write_stderr() {
    let d = TestDaemon::spawn();

    comb(&d)
        .args(["put", "myvirtual2", r#"{"field1":"hello"}"#, "--path", "/"])
        .assert()
        .success();

    let out = comb(&d)
        .args(["get", "myvirtual2.nosuchfield", "-f", "json", "--path", "/"])
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "querying a nonexistent field must fail, not silently succeed"
    );
    assert!(
        !out.stderr.is_empty(),
        "a non-zero exit from a single-key get must always explain itself on stderr"
    );
}

// ── golden_watch_emits_initial_value_then_exits ───────────────────────────────

#[test]
fn golden_watch_emits_value_then_exits() {
    let d = TestDaemon::spawn();

    // Seed a value so the watch has something to emit immediately.
    comb(&d)
        .args(["put", "watchprov", r#"{"val":"stream-me"}"#, "--path", "/"])
        .assert()
        .success();

    // `watch` streams forever; kill it after 2 s.  The initial value line is
    // written before the process is killed, so stdout will contain it even
    // though the exit code is non-zero (killed by timeout).
    // `--path /` matches the `--path /` used in the put above.
    comb(&d)
        .args(["watch", "watchprov.val", "--path", "/"])
        .timeout(Duration::from_secs(2))
        .assert()
        .stdout(predicates::str::contains("stream-me"));
}

// ── golden_eval_renders_template ─────────────────────────────────────────────

#[test]
fn golden_eval_renders_template() {
    let d = TestDaemon::spawn();

    // A template with no provider.field references is rendered directly without
    // touching the daemon — cheapest golden path.
    //
    // The source needs at least one tag to be a *template*: canon invariant 14
    // makes a source with no tags a bare expression, so the pre-Task-3
    // `"hello world"` is now two identifiers juxtaposed — an expression syntax
    // error, not literal text. `{{ }}` is how you say "this is text".
    comb(&d)
        .args(["eval", "hello {{ 'world' }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("hello world"));
}

// ── golden_check_daemon ───────────────────────────────────────────────────────

#[test]
fn golden_check_runs_diagnostics() {
    let d = TestDaemon::spawn();

    // `comb check daemon` contacts the running daemon and prints PASS/INFO lines.
    // Exit code is 0 when the daemon responds cleanly.
    comb(&d)
        .args(["check", "daemon"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Daemon"));
}

// ── golden_kill_terminates_daemon ─────────────────────────────────────────────

#[test]
fn golden_kill_terminates_daemon() {
    let d = TestDaemon::spawn();

    // `kill` needs --socket because it derives the pid file path from it.
    // The socket flag is only accepted on the Kill subcommand, not at top level.
    assert_cmd::Command::cargo_bin("comb")
        .unwrap()
        .env("BEACHCOMBER_SOCKET", &d.socket.path)
        .env("RUST_LOG", "error")
        .args(["kill", "--socket"])
        .arg(&d.socket.path)
        .assert()
        .success();

    // After kill the socket should no longer accept connections.
    assert!(
        !d.socket.path.exists() || {
            use std::os::unix::net::UnixStream;
            UnixStream::connect(&d.socket.path).is_err()
        }
    );
}

// ── golden_init_prints_or_writes_config ──────────────────────────────────────

#[test]
fn golden_init_prints_or_writes_config() {
    let d = TestDaemon::spawn();

    // `comb init` detects installed tools and prints suggestions.
    // It doesn't contact the daemon, so it always exits 0.
    comb(&d).args(["init"]).assert().success();
}

// ── one expression syntax: `comb eval` over all three forms ──────────────────
//
// Task 3 routed `run_eval` through `libbeachcomber::eval`, so `comb eval`
// accepts every form canon `field_resolution.md` invariant 14 defines: a bare
// expression, exactly one `{{ expr }}` (natural type), and a template (string).
// A typed result prints through `libbeachcomber::render::render_data` — the
// same renderer `comb get -f text` uses — so `comb eval '{{ p.f }}'` and
// `comb get p.f` agree on how a value looks.

/// A `comb` command with an isolated, empty config dir, so the ambient
/// `~/.config/beachcomber/config.toml` cannot leak virtual fields into a test.
/// `cfg` may hold a `beachcomber/config.toml` written by the caller.
fn comb_with_config(d: &TestDaemon, cfg: &std::path::Path) -> assert_cmd::Command {
    let mut cmd = comb(d);
    cmd.env("XDG_CONFIG_HOME", cfg);
    cmd
}

/// Write `body` as the beachcomber config inside `dir`, returning the
/// `XDG_CONFIG_HOME` to hand to [`comb_with_config`].
fn write_config(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    let cfg = dir.join("cfg");
    std::fs::create_dir_all(cfg.join("beachcomber")).unwrap();
    std::fs::write(cfg.join("beachcomber").join("config.toml"), body).unwrap();
    cfg
}

#[test]
fn eval_single_tag_prints_raw_string() {
    let d = TestDaemon::spawn();

    comb(&d)
        .args(["put", "evalprov", r#"{"s":"hello"}"#])
        .assert()
        .success();

    // Exactly one tag → the expression's natural type (a string), printed raw
    // with no trailing newline — byte-identical to the pre-Task-3 rendering.
    comb(&d)
        .args(["eval", "{{ evalprov.s }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("hello"));
}

#[test]
fn eval_bare_expression_prints_value() {
    let d = TestDaemon::spawn();

    comb(&d)
        .args(["put", "evalbare", r#"{"s":"hello"}"#])
        .assert()
        .success();

    // No tags at all → the whole source is the expression. Before Task 3 this
    // printed the literal text `evalbare.s`.
    comb(&d)
        .args(["eval", "evalbare.s"])
        .assert()
        .success()
        .stdout(predicates::str::diff("hello"));
}

#[test]
fn eval_template_with_literal_text() {
    let d = TestDaemon::spawn();

    comb(&d)
        .args(["put", "evaltmpl", r#"{"s":"main"}"#])
        .assert()
        .success();

    // Literal text around the tag → string-valued, rendered as written.
    comb(&d)
        .args(["eval", "branch: {{ evaltmpl.s }}!"])
        .assert()
        .success()
        .stdout(predicates::str::diff("branch: main!"));
}

#[test]
fn eval_single_tag_bool_prints_true() {
    let d = TestDaemon::spawn();

    comb(&d)
        .args(["put", "evalbool", r#"{"b":true,"c":false}"#])
        .assert()
        .success();

    comb(&d)
        .args(["eval", "{{ evalbool.b }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("true"));

    comb(&d)
        .args(["eval", "{{ evalbool.c }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("false"));
}

#[test]
fn eval_single_tag_object_prints_like_get_text() {
    let d = TestDaemon::spawn();

    // `--path /` matches the "/" CWD the `comb()` helper fixes, so the
    // server-side `-f text` path finds the same entry `eval` does.
    comb(&d)
        .args([
            "put",
            "evalobj",
            r#"{"o":{"a":1,"b":"two"}}"#,
            "--path",
            "/",
        ])
        .assert()
        .success();

    // An object keeps its type through the single-tag form and prints through
    // `render_data` — sorted `key=value` lines, exactly what `comb get -f text`
    // gives for the same key. Before Task 3 this was minijinja's map
    // formatting (`{"a": 1, "b": "two"}`).
    let expected = comb(&d).args(["get", "evalobj.o"]).output().unwrap();
    let expected = String::from_utf8(expected.stdout).unwrap();
    assert_eq!(expected, "a=1\nb=two");

    comb(&d)
        .args(["eval", "{{ evalobj.o }}", "/"])
        .assert()
        .success()
        .stdout(predicates::str::diff("a=1\nb=two"));
}

#[test]
fn eval_virtual_field_defined_with_tags_in_config() {
    let d = TestDaemon::spawn();
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = write_config(
        dir.path(),
        r#"
[providers.x]
virtual.y = "{{ env.A }}"
"#,
    );

    // A config virtual field written with tags resolves the same way through
    // `get` and through `eval`.
    comb_with_config(&d, &cfg)
        .env("A", "from-env")
        .args(["get", "x.y"])
        .assert()
        .success()
        .stdout(predicates::str::contains("from-env"));

    comb_with_config(&d, &cfg)
        .env("A", "from-env")
        .args(["eval", "{{ x.y }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("from-env"));
}

#[test]
fn eval_nested_virtual_dependency_is_fetched() {
    let d = TestDaemon::spawn();
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = write_config(
        dir.path(),
        r#"
[providers.x]
virtual.a = "{{ b.y or cache.c.z }}"

[providers.b]
virtual.y = "{{ cache.d.w }}"
"#,
    );

    comb(&d)
        .args(["put", "d", r#"{"w":"deep-value"}"#])
        .assert()
        .success();

    // `x.a` → `b.y` (virtual) → `cache.d.w`: the daemon-ref closure must reach
    // through two levels of virtual field and actually fetch `d.w`.
    comb_with_config(&d, &cfg)
        .args(["eval", "{{ x.a }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("deep-value"));
}

#[test]
fn eval_env_only_template_does_not_need_daemon() {
    // No daemon at all: the socket path's parent is a regular file, so one
    // cannot even be started. A source referencing only `env.*` has no daemon
    // refs, so `ensure_daemon` is never called.
    let dir = tempfile::TempDir::new().unwrap();
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a dir").unwrap();
    let unstartable = blocker.join("sock");

    assert_cmd::Command::cargo_bin("comb")
        .unwrap()
        .env("BEACHCOMBER_SOCKET", &unstartable)
        .env("RUST_LOG", "error")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("cfg"))
        .env("FOO", "hi")
        .args(["eval", "env.FOO is {{ env.FOO }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("env.FOO is hi"));
}

// ── -f fmt and the status formatter ──────────────────────────────────────────

#[test]
fn fmt_renders_literal_filter_and_conditional() {
    let d = TestDaemon::spawn();

    comb(&d)
        .args(["put", "fmtprov", r#"{"val":"formatted","n":3}"#])
        .assert()
        .success();

    // `-f fmt` (reached through the `.f` suffix syntax) renders through
    // `eval::render_template` after Task 3. Literal text, filters and a
    // conditional must all still behave exactly as they did under `render_str`.
    comb(&d)
        .args(["g.f", "[{{ val }}/{{ n }}]", "fmtprov"])
        .assert()
        .success()
        .stdout(predicates::str::diff("[formatted/3]"));

    comb(&d)
        .args([
            "g.f",
            "{% if n %}{{ val | truncate(4) }}{% endif %}",
            "fmtprov",
        ])
        .assert()
        .success()
        .stdout(predicates::str::diff("form..."));
}

// ── eval_default_filter_fires_on_daemon_miss ─────────────────────────────────

#[test]
fn eval_default_filter_fires_on_daemon_miss() {
    let d = TestDaemon::spawn();

    // A daemon-backed ref that misses leaves the key absent from the render
    // context, so it is *undefined* and `default` fires. Binding a miss to JSON
    // null instead would make `default` a no-op and print nothing.
    comb(&d)
        .args(["eval", r#"{{ probe.missing | default("FB") }}"#])
        .assert()
        .success()
        .stdout(predicates::str::diff("FB"));

    // The bare and template forms agree.
    comb(&d)
        .args(["eval", r#"probe.missing | default("FB")"#])
        .assert()
        .success()
        .stdout(predicates::str::diff("FB"));

    comb(&d)
        .args(["eval", r#"[{{ probe.missing | default("FB") }}]"#])
        .assert()
        .success()
        .stdout(predicates::str::diff("[FB]"));

    // A hit still wins over the default.
    comb(&d)
        .args(["put", "probehit", r#"{"f":"real"}"#])
        .assert()
        .success();
    comb(&d)
        .args(["eval", r#"{{ probehit.f | default("FB") }}"#])
        .assert()
        .success()
        .stdout(predicates::str::diff("real"));
}

#[test]
fn fmt_missing_and_null_render_empty() {
    let d = TestDaemon::spawn();

    comb(&d)
        .args(["put", "nullfmt", r#"{"v":"x","n":null}"#, "--path", "/"])
        .assert()
        .success();

    // An unbound name chained into is undefined all the way down, and renders
    // empty rather than erroring (`UndefinedBehavior::Chainable`).
    comb(&d)
        .args(["g.f", "[{{ nope.sub }}]", "nullfmt"])
        .assert()
        .success()
        .stdout(predicates::str::diff("[]"));

    // A field that really is JSON null renders empty too, not the word `none`.
    comb(&d)
        .args(["g.f", "[{{ n }}]", "nullfmt"])
        .assert()
        .success()
        .stdout(predicates::str::diff("[]"));

    // Same rule in the status formatter: a global row's null `path` is empty.
    comb(&d)
        .args(["put", "globalfmt", r#"{"g":"v"}"#])
        .assert()
        .success();
    comb(&d)
        .args([
            "status",
            "-f",
            "[{{ path }}]",
            "--filter",
            "provider=globalfmt",
        ])
        .assert()
        .success()
        .stdout(predicates::str::diff("[]\n"));
}

// ── eval_plain_text_without_tags_is_an_expression_error ──────────────────────

#[test]
fn eval_plain_text_without_tags_is_an_expression_error() {
    let d = TestDaemon::spawn();

    // Canon invariant 14: a source with no tags is a bare *expression*, so
    // plain prose is two identifiers juxtaposed — a syntax error, not literal
    // text. `{{ }}` is how you say "this is text".
    comb(&d)
        .args(["eval", "hello world"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicates::str::contains("expression compile error"));
}
