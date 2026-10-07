use crate::common::daemon::TestDaemon;
use std::path::Path;
use std::time::Duration;

#[test]
fn daemon_fixture_starts_and_exits() {
    let d = TestDaemon::spawn();
    assert!(d.socket.path.exists());
    drop(d);
}

// ── helper ────────────────────────────────────────────────────────────────────

// ── golden_get_returns_known_value ────────────────────────────────────────────

#[test]
fn golden_get_returns_known_value() {
    let d = TestDaemon::spawn();

    // Seed a virtual key.  `get` always injects CWD as path context for virtual
    // providers; we fix CWD to "/" (every `comb` here runs at "/") so we must also put
    // with `--path /` to store under the same cache key.
    d.comb(Path::new("/"))
        .args(["put", "myapp", r#"{"status":"healthy"}"#, "--path", "/"])
        .assert()
        .success();

    d.comb(Path::new("/"))
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
    d.comb(Path::new("/")).args(["status"]).assert().success();
}

// ── golden_put_sets_a_value ───────────────────────────────────────────────────

#[test]
fn golden_put_sets_a_value() {
    let d = TestDaemon::spawn();

    // put exits 0; a subsequent get returns the stored value.
    // `--path /` matches the "/" CWD injected by get's path resolution.
    d.comb(Path::new("/"))
        .args(["put", "testprov", r#"{"field":"golden"}"#, "--path", "/"])
        .assert()
        .success();

    d.comb(Path::new("/"))
        .args(["get", "testprov.field"])
        .assert()
        .success()
        .stdout(predicates::str::contains("golden"));
}

// ── get_json_finds_put_created_virtual_without_explicit_path ─────────────────

/// A `put` with no `--path` and a `get` with no `--path` must agree on where
/// "no path" means. Both resolve through `effective_cli_path` in `main.rs`,
/// which defaults an absent path (with no `--global`) to the process's
/// current directory — `comb` runs at "/" — so `put myvirtual
/// '{"field1":"hello"}'` and `get myvirtual.field1 -f json` land at and read
/// from the same entry without either command naming a directory.
#[test]
fn get_json_finds_put_created_virtual_without_explicit_path() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args(["put", "myvirtual", r#"{"field1":"hello"}"#])
        .assert()
        .success();

    d.comb(Path::new("/"))
        .args(["get", "myvirtual.field1", "-f", "json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("hello"));
}

// ── single_key_get_error_paths_always_write_stderr ────────────────────────────

/// Regression test: every branch of `run_get`'s per-key `resolve()` loop that
/// sets the error flag must also write something to stderr. A miss under a
/// structured format takes the `Ok(None)` arm, which prints "no data (cache
/// miss)"; before the fix that arm set the flag with no accompanying
/// `eprintln!`, so the process exited 2 with zero bytes on both stdout and
/// stderr — indistinguishable from a hang or a swallowed panic. This exercises
/// a genuine miss (an unknown field on an existing static namespace,
/// path-matched on both ends) rather than a routing bug, to isolate the
/// "silent failure" defect from "wrong routing".
#[test]
fn single_key_get_error_paths_always_write_stderr() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args(["put", "myvirtual2", r#"{"field1":"hello"}"#, "--path", "/"])
        .assert()
        .success();

    let out = d
        .comb(Path::new("/"))
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

// ── get_text_finds_put_created_virtual_without_explicit_path ─────────────────

/// Companion to the json test above, for the default `text` format: a `put`
/// with no `--path` (stored at the caller's cwd) must be visible to a `get`
/// with no `--path` from the same cwd here too. `run_get` resolves every
/// format through the same `resolve()` call per key, so there is no separate
/// route for `text`/`sh` to drop the value on — this covers a scalar field, a
/// nested field, and the bare provider (flattened `key=value` lines).
#[test]
fn get_text_finds_put_created_virtual_without_explicit_path() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args([
            "put",
            "textvirt",
            r#"{"stdin":"{\"session_id\":\"abc\"}","context":{"pct":42}}"#,
        ])
        .assert()
        .success();

    // Scalar field.
    d.comb(Path::new("/"))
        .args(["get", "textvirt.stdin"])
        .assert()
        .success()
        .stdout(predicates::str::contains(r#"{"session_id":"abc"}"#));

    // Nested field.
    d.comb(Path::new("/"))
        .args(["get", "textvirt.context.pct"])
        .assert()
        .success()
        .stdout(predicates::str::contains("42"));

    // Whole provider: sorted `key=value` lines, nested objects flattened.
    d.comb(Path::new("/"))
        .args(["get", "textvirt"])
        .assert()
        .success()
        .stdout(predicates::str::contains("context.pct=42"))
        .stdout(predicates::str::contains(r#"stdin={"session_id":"abc"}"#));
}

// ── get_sh_finds_put_created_virtual_without_explicit_path ───────────────────

/// The `sh` output format renders through the same `render::render_data` as
/// text, so it lost put-created values the same way. Companion to the text
/// test above.
#[test]
fn get_sh_finds_put_created_virtual_without_explicit_path() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args([
            "put",
            "shvirt",
            r#"{"stdin":"raw-blob","context":{"pct":42}}"#,
        ])
        .assert()
        .success();

    // `sh` shell-quotes every value it renders (canon field_resolution.md
    // invariant 8), so the expected forms carry their quotes.
    d.comb(Path::new("/"))
        .args(["get", "-f", "sh", "shvirt.stdin"])
        .assert()
        .success()
        .stdout(predicates::str::contains("'raw-blob'"));

    d.comb(Path::new("/"))
        .args(["get", "-f", "sh", "shvirt"])
        .assert()
        .success()
        .stdout(predicates::str::contains("context.pct='42'"));
}

// ── multi_key_get_text_finds_put_created_virtual_without_explicit_path ───────

/// The multi-key text/sh loop loses the same value by a different mechanism:
/// it issues each get with no request path, but on a session whose context was
/// already set to the CLI's ambient CWD default, which scopes the lookup away
/// from the globally-stored entry just as an explicit path would. A daemon
/// provider queried alongside it still answers, so the miss is silent.
#[test]
fn multi_key_get_text_finds_put_created_virtual_without_explicit_path() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args(["put", "multivirt", r#"{"field1":"multi-hello"}"#])
        .assert()
        .success();

    d.comb(Path::new("/"))
        .args(["get", "multivirt.field1", "hostname.name"])
        .assert()
        .success()
        .stdout(predicates::str::contains("multi-hello"));
}

// ── golden_watch_emits_initial_value_then_exits ───────────────────────────────

#[test]
fn golden_watch_emits_value_then_exits() {
    let d = TestDaemon::spawn();

    // Seed a value so the watch has something to emit immediately.
    d.comb(Path::new("/"))
        .args(["put", "watchprov", r#"{"val":"stream-me"}"#, "--path", "/"])
        .assert()
        .success();

    // `watch` streams forever; kill it after 2 s.  The initial value line is
    // written before the process is killed, so stdout will contain it even
    // though the exit code is non-zero (killed by timeout).
    // `--path /` matches the `--path /` used in the put above.
    d.comb(Path::new("/"))
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
    // touching the daemon — cheapest golden path. This source carries a tag so
    // it also exercises the template-render path, not just literal text.
    d.comb(Path::new("/"))
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
    d.comb(Path::new("/"))
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
    d.comb(Path::new("/"))
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
    d.comb(Path::new("/")).args(["init"]).assert().success();
}

// ── one expression syntax: `comb eval` over both forms ────────────────────────
//
// `comb eval` accepts both forms canon `field_resolution.md` invariant 14
// defines: exactly one `{{ expr }}` tag (natural type) or literal text and/or
// several tags (string) — a source with no tags at all is its literal text.
// A typed result prints through `libbeachcomber::render::render_data` — the
// same renderer `comb get -f text` uses — so `comb eval '{{ p.f }}'` and
// `comb get p.f` agree on how a value looks.

#[test]
fn eval_single_tag_prints_raw_string() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args(["put", "evalprov", r#"{"s":"hello"}"#])
        .assert()
        .success();

    // Exactly one tag → the expression's natural type (a string), printed raw
    // with no trailing newline — byte-identical to the pre-Task-3 rendering.
    d.comb(Path::new("/"))
        .args(["eval", "{{ evalprov.s }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("hello"));
}

#[test]
fn eval_text_with_no_tags_prints_itself_even_when_it_looks_like_a_key() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args(["put", "evalbare", r#"{"s":"hello"}"#])
        .assert()
        .success();

    // No tags at all → literal text, not the expression `evalbare.s` — it
    // prints itself even though a matching field exists. `{{ }}` is how you
    // say "evaluate this".
    d.comb(Path::new("/"))
        .args(["eval", "evalbare.s"])
        .assert()
        .success()
        .stdout(predicates::str::diff("evalbare.s"));
}

#[test]
fn eval_template_with_literal_text() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args(["put", "evaltmpl", r#"{"s":"main"}"#])
        .assert()
        .success();

    // Literal text around the tag → string-valued, rendered as written.
    d.comb(Path::new("/"))
        .args(["eval", "branch: {{ evaltmpl.s }}!"])
        .assert()
        .success()
        .stdout(predicates::str::diff("branch: main!"));
}

#[test]
fn eval_single_tag_bool_prints_true() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args(["put", "evalbool", r#"{"b":true,"c":false}"#])
        .assert()
        .success();

    d.comb(Path::new("/"))
        .args(["eval", "{{ evalbool.b }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("true"));

    d.comb(Path::new("/"))
        .args(["eval", "{{ evalbool.c }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("false"));
}

#[test]
fn eval_single_tag_object_prints_like_get_text() {
    let d = TestDaemon::spawn();

    // `--path /` matches the "/" CWD these tests run `comb` at, so the
    // server-side `-f text` path finds the same entry `eval` does.
    d.comb(Path::new("/"))
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
    let expected = d
        .comb(Path::new("/"))
        .args(["get", "evalobj.o"])
        .output()
        .unwrap();
    let expected = String::from_utf8(expected.stdout).unwrap();
    assert_eq!(expected, "a=1\nb=two");

    d.comb(Path::new("/"))
        .args(["eval", "{{ evalobj.o }}", "/"])
        .assert()
        .success()
        .stdout(predicates::str::diff("a=1\nb=two"));
}

#[test]
fn get_of_a_template_written_with_tags_renders_it() {
    let d = TestDaemon::spawn();

    // A template is data the daemon holds under `x:template`; `get x.y`
    // renders it with the caller's environment.
    d.comb(Path::new("/"))
        .args(["put", "x:template.y", r#""{{ env.A }}""#, "--path", "/"])
        .assert()
        .success();

    d.comb(Path::new("/"))
        .env("A", "from-env")
        .args(["get", "x.y"])
        .assert()
        .success()
        .stdout(predicates::str::diff("from-env"));

    // `comb eval '{{ x.y }}'` resolves the same ref through the same
    // library resolver, so it renders the identical value `get` does.
    d.comb(Path::new("/"))
        .env("A", "from-env")
        .args(["eval", "{{ x.y }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("from-env"));
}

#[test]
fn eval_nested_virtual_dependency_is_fetched() {
    let d = TestDaemon::spawn();

    // `x.a` is a template naming `b.y`, itself a template naming `comb.d.w`:
    // the resolver must reach through two levels of stored template and
    // actually fetch `d.w`. `comb.c.z` names no such provider and is falsy.
    d.comb(Path::new("/"))
        .args(["put", "x:template.a", r#""{{ b.y or comb.c.z }}""#])
        .assert()
        .success();
    d.comb(Path::new("/"))
        .args(["put", "b:template.y", r#""{{ comb.d.w }}""#])
        .assert()
        .success();
    d.comb(Path::new("/"))
        .args(["put", "d", r#"{"w":"deep-value"}"#])
        .assert()
        .success();

    d.comb(Path::new("/"))
        .args(["eval", "{{ x.a }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("deep-value"));
}

// ── -f fmt and the status formatter ──────────────────────────────────────────

#[test]
fn fmt_renders_literal_filter_and_conditional() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args(["put", "fmtprov", r#"{"val":"formatted","n":3}"#])
        .assert()
        .success();

    // `-f fmt` (reached through the `.f` suffix syntax) renders through
    // `eval::render_template` after Task 3. Literal text, filters and a
    // conditional must all still behave exactly as they did under `render_str`.
    d.comb(Path::new("/"))
        .args(["g.f", "[{{ val }}/{{ n }}]", "fmtprov"])
        .assert()
        .success()
        .stdout(predicates::str::diff("[formatted/3]"));

    d.comb(Path::new("/"))
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
    d.comb(Path::new("/"))
        .args(["eval", r#"{{ probe.missing | default("FB") }}"#])
        .assert()
        .success()
        .stdout(predicates::str::diff("FB"));

    d.comb(Path::new("/"))
        .args(["eval", r#"[{{ probe.missing | default("FB") }}]"#])
        .assert()
        .success()
        .stdout(predicates::str::diff("[FB]"));

    // A hit still wins over the default.
    d.comb(Path::new("/"))
        .args(["put", "probehit", r#"{"f":"real"}"#])
        .assert()
        .success();
    d.comb(Path::new("/"))
        .args(["eval", r#"{{ probehit.f | default("FB") }}"#])
        .assert()
        .success()
        .stdout(predicates::str::diff("real"));
}

#[test]
fn fmt_missing_and_null_render_empty() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args(["put", "nullfmt", r#"{"v":"x","n":null}"#, "--path", "/"])
        .assert()
        .success();

    // An unbound name chained into is undefined all the way down, and renders
    // empty rather than erroring (`UndefinedBehavior::Chainable`).
    d.comb(Path::new("/"))
        .args(["g.f", "[{{ nope.sub }}]", "nullfmt"])
        .assert()
        .success()
        .stdout(predicates::str::diff("[]"));

    // A field that really is JSON null renders empty too, not the word `none`.
    d.comb(Path::new("/"))
        .args(["g.f", "[{{ n }}]", "nullfmt"])
        .assert()
        .success()
        .stdout(predicates::str::diff("[]"));

    // Same rule in the status formatter: a global row's null `path` is empty.
    // `put` defaults to the cwd like `get`, so the global entry needs asking for.
    d.comb(Path::new("/"))
        .args(["put", "--global", "globalfmt", r#"{"g":"v"}"#])
        .assert()
        .success();
    d.comb(Path::new("/"))
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

// ── eval_plain_text_without_tags_prints_itself ────────────────────────────────

#[test]
fn eval_plain_text_without_tags_prints_itself() {
    let d = TestDaemon::spawn();

    // Canon invariant 14: a source with no tags at all is its own literal
    // text. `{{ }}` is how you say "evaluate this".
    d.comb(Path::new("/"))
        .args(["eval", "hello world"])
        .assert()
        .success()
        .stdout(predicates::str::diff("hello world"));
}
