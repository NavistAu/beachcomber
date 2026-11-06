//! End-to-end tests for `comb watch`: its path handling (a cwd default, a
//! positional path, `--path`, and `--global`, like `get`/`refresh` — Plan 4
//! Task 1) and its resolved semantics (Plan 4 Task 3 — every frame is the
//! value `comb get` would print, re-resolved over every key the resolution
//! reads plus the provider's template tree).
//!
//! `comb watch` streams forever, so a streaming case here spawns it as a
//! child and reads its lines through `common::watch`, then kills it. The two error cases
//! (conflicting path spellings, a deleted cwd) exit before any streaming
//! starts, so they use `assert_cmd` like `tests/suite/cli_refresh.rs` does
//! for the same cases.

use crate::common::daemon::{TestDaemon, isolate};
use crate::common::watch::{first_line, watch_lines};

/// The template tree these tests watch through: one leaf, rendered from a
/// literal tag so no environment is involved.
const TREE: &str = r#"{"widgets":{"foo":"{{ 'hi' }}"}}"#;

/// `TREE` with the leaf edited — what a `put` to the template makes the
/// watched value become.
const TREE_EDITED: &str = r#"{"widgets":{"foo":"{{ 'hi!' }}"}}"#;

#[test]
fn watch_defaults_to_the_callers_cwd() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    std::fs::create_dir_all(&project).unwrap();

    d.comb(&project)
        .args(["put", "my", r#"{"a":1}"#])
        .assert()
        .success();

    assert_eq!(first_line(&d, &project, &["my.a"]), "1");
}

/// A put scoped to `project` doesn't merge into `other`'s chain, so a watch
/// at `other` starts on a miss — and a text watch prints no line for a miss.
/// The first line to arrive is therefore `other`'s own later value, which
/// pins both halves at once: no blank frame came first, and the `1` at the
/// sibling was never visible.
#[test]
fn watch_from_a_sibling_directory_does_not_see_that_directorys_value() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    let other = d.home.join("ws/other");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&other).unwrap();

    d.comb(&project)
        .args(["put", "my", r#"{"a":1}"#])
        .assert()
        .success();

    let mut w = watch_lines(&d, &other, &["my.a"]);
    d.comb(&other)
        .args(["put", "my", r#"{"a":2}"#])
        .assert()
        .success();
    assert_eq!(w.next_line(), "2");
}

#[test]
fn watch_global_reads_the_home_entry_from_anywhere() {
    let d = TestDaemon::spawn();
    let elsewhere = d.home.join("ws/other");
    std::fs::create_dir_all(&elsewhere).unwrap();

    d.comb(&d.home)
        .args(["put", "--global", "g", r#"{"v":2}"#])
        .assert()
        .success();

    assert_eq!(first_line(&d, &elsewhere, &["--global", "g.v"]), "2");
}

#[test]
fn watch_takes_a_relative_positional_path() {
    let d = TestDaemon::spawn();
    let ws = d.home.join("ws");
    let project = ws.join("project");
    std::fs::create_dir_all(&project).unwrap();

    d.comb(&project)
        .args(["put", "my", r#"{"a":1}"#])
        .assert()
        .success();

    assert_eq!(first_line(&d, &ws, &["my.a", "./project"]), "1");
}

#[test]
fn watch_takes_the_path_as_a_flag() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    std::fs::create_dir_all(&project).unwrap();

    d.comb(&project)
        .args(["put", "my", r#"{"a":1}"#])
        .assert()
        .success();

    let project_str = project.to_str().unwrap();
    assert_eq!(
        first_line(&d, &d.home, &["my.a", "--path", project_str]),
        "1"
    );
}

/// The cwd is read only when it is needed, and when it is needed and gone the
/// answer is an error — never a silent watch of the global entry.
#[test]
fn watch_from_a_deleted_cwd_with_no_path_is_an_error() {
    let d = TestDaemon::spawn();
    let gone = d.home.join("gone");
    std::fs::create_dir_all(&gone).unwrap();

    // `sh` enters the directory, removes it from under itself, and execs
    // `comb` there; `Command::current_dir` on a missing directory would fail
    // at spawn instead.
    let mut cmd = std::process::Command::new("sh");
    isolate(&mut cmd, &d.home, &d.xdg, &d.socket.path)
        .env("COMB", env!("CARGO_BIN_EXE_comb"))
        .args([
            "-c",
            r#"cd "$1" && rmdir "$1" && exec "$COMB" watch my.a"#,
            "sh",
        ])
        .arg(&gone);
    assert_cmd::Command::from_std(cmd)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("error: current directory: "));
}

/// The same deleted-cwd case, but with an explicit relative positional (`.`)
/// rather than relying on the default — covers the relative branch of
/// `effective_cli_path`, where `watch_from_a_deleted_cwd_with_no_path_is_an_error`
/// above covers the `None` branch.
#[test]
fn watch_from_a_deleted_cwd_with_a_relative_path_is_an_error() {
    let d = TestDaemon::spawn();
    let gone = d.home.join("gone");
    std::fs::create_dir_all(&gone).unwrap();

    // `sh` enters the directory, removes it from under itself, and execs
    // `comb` there; `Command::current_dir` on a missing directory would fail
    // at spawn instead.
    let mut cmd = std::process::Command::new("sh");
    isolate(&mut cmd, &d.home, &d.xdg, &d.socket.path)
        .env("COMB", env!("CARGO_BIN_EXE_comb"))
        .args([
            "-c",
            r#"cd "$1" && rmdir "$1" && exec "$COMB" watch my.a ."#,
            "sh",
        ])
        .arg(&gone);
    assert_cmd::Command::from_std(cmd)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("error: current directory: "));
}

// ── Plan 4 Task 3: every frame is the resolved value ────────────────────────

/// The watched value is what `comb get` would print — a template leaf
/// rendered, not the raw tag — and editing the template the value is
/// rendered from fires the watch again with the new rendering. The template
/// tree is not a key the addressed watch would ever cover on its own: the
/// fan-in subscribes to it because the resolution read it.
#[test]
fn watch_emits_the_resolved_value_and_refires_when_its_template_changes() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    std::fs::create_dir_all(&project).unwrap();

    d.comb(&project)
        .args(["put", "scanline:template", TREE])
        .assert()
        .success();

    let mut w = watch_lines(&d, &project, &["scanline.widgets.foo"]);
    assert_eq!(w.next_line(), "hi");

    d.comb(&project)
        .args(["put", "scanline:template", TREE_EDITED])
        .assert()
        .success();
    assert_eq!(w.next_line(), "hi!");
}

/// A watch on a cached leaf follows its memo: a dropped hash (`comb refresh`)
/// re-renders it, the render writes the memo back, and the same value is not
/// printed again; an edit of the template is a new value, printed.
#[test]
fn a_watch_rerenders_when_a_memo_hash_drops() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    std::fs::create_dir_all(&project).unwrap();
    let put = |key: &str, data: &str| {
        d.comb(&project).args(["put", key, data]).assert().success();
    };
    let hash = || {
        let out = d
            .comb(&project)
            .args(["get", "-f", "json", "p:cache.f"])
            .output()
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap_or_default()["hash"]
            .as_str()
            .map(str::to_string)
    };
    put("d.v", r#""a""#);
    put("p:template.f", r#""{{ comb.d.v | cache() | upper }}""#);

    let mut w = watch_lines(&d, &project, &["p.f"]);
    assert_eq!(w.next_line(), "A");
    // The opening read stored the memo before it printed.
    let stored = hash().expect("the watch's read stored a memo");

    d.comb(&project).args(["refresh", "p"]).assert().success();
    // Only the watch reads `p.f`: the hash comes back when its re-render
    // writes the memo again.
    crate::common::poll::poll_until(
        "p:cache.f's hash",
        "written back by the watch's re-render",
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(5),
        hash,
        |h| h.is_some(),
    );
    assert_eq!(hash(), Some(stored), "the same inputs, the same hash");

    put(
        "p:template.f",
        r#""{{ comb.d.v | cache() | upper ~ '!' }}""#,
    );
    assert_eq!(w.next_line(), "A!", "no frame for the unchanged re-render");
}

/// A library a watched template imports is a dependency of the watch: a put
/// that edits the library re-renders the watched value with the new text.
#[test]
fn a_library_edit_rerenders_a_watch() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    std::fs::create_dir_all(&project).unwrap();
    let library = |text: &str| format!("\"{{% macro m() %}}{text}{{% endmacro %}}\"");

    d.comb(&project)
        .args(["put", "--global", "scanline:macro.lib", &library("one")])
        .assert()
        .success();
    d.comb(&project)
        .args([
            "put",
            "scanline:template",
            r#"{"w":"{% from 'scanline:macro.lib' import m %}{{ m() }}"}"#,
        ])
        .assert()
        .success();

    let mut w = watch_lines(&d, &project, &["scanline.w"]);
    assert_eq!(w.next_line(), "one");

    d.comb(&project)
        .args(["put", "--global", "scanline:macro.lib", &library("two")])
        .assert()
        .success();
    assert_eq!(w.next_line(), "two");
}

/// `env.*` is constant for the life of a process, so there is nothing to
/// watch and the library says so before any connection is made. The CLI
/// prints the library's words and exits 2.
#[test]
fn watch_refuses_an_env_key() {
    let d = TestDaemon::spawn();

    d.comb(&d.home)
        .args(["watch", "env.HOME"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("env keys cannot be watched"));
}

/// A facet key is already inside a facet: there is no template half to
/// render, so its frames are the daemon's own — `scanline:template` streams
/// the raw tree, tags and all, exactly as `comb get scanline:template -f
/// json` prints it.
#[test]
fn watch_on_a_template_facet_streams_the_raw_tree() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    std::fs::create_dir_all(&project).unwrap();

    d.comb(&project)
        .args(["put", "scanline:template", TREE])
        .assert()
        .success();

    let line = first_line(&d, &project, &["scanline:template", "-f", "json"]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap(),
        serde_json::json!({"widgets": {"foo": "{{ 'hi' }}"}})
    );
}

/// `-f json` frames are the bare value, one JSON document per line — not the
/// `{"ok":…,"data":…,"age_ms":…}` response envelope the daemon puts on the
/// wire, which is what `watch` used to print.
#[test]
fn watch_json_frames_are_bare_values() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    std::fs::create_dir_all(&project).unwrap();

    d.comb(&project)
        .args(["put", "scanline:template", TREE])
        .assert()
        .success();

    let mut w = watch_lines(&d, &project, &["scanline.widgets.foo", "-f", "json"]);
    assert_eq!(w.next_line(), r#""hi""#);

    d.comb(&project)
        .args(["put", "scanline:template", TREE_EDITED])
        .assert()
        .success();
    assert_eq!(w.next_line(), r#""hi!""#);
}

// ── A miss is a frame as `get` prints one ──────────────────────────────────

/// `comb get -f json` prints `null` for a miss, so a json watch does too: the
/// first frame on a key nothing answers is `null`, and the value follows when
/// a `put` gives it one.
#[test]
fn json_watch_on_a_missing_key_prints_null_then_the_value() {
    let d = TestDaemon::spawn();

    let mut w = watch_lines(&d, &d.home, &["--global", "my.a", "-f", "json"]);
    assert_eq!(w.next_line(), "null");

    d.comb(&d.home)
        .args(["put", "--global", "my", r#"{"a":1}"#])
        .assert()
        .success();
    assert_eq!(w.next_line(), "1");
}

/// `comb get -f '<template>'` renders the template for a miss, so a template
/// watch does too: the first frame on a key nothing answers is the template's
/// text, and the value's rendering follows when a `put` gives it one.
#[test]
fn template_watch_on_a_missing_key_renders_the_template_then_the_value() {
    let d = TestDaemon::spawn();
    let tpl = "{{ value if value is defined else 'none' }}";

    let got = d
        .comb(&d.home)
        .args(["get", "--global", "my.a", "-f", tpl])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&got.stdout), "none");

    let mut w = watch_lines(&d, &d.home, &["--global", "my.a", "-f", tpl]);
    assert_eq!(w.next_line(), "none");

    d.comb(&d.home)
        .args(["put", "--global", "my", r#"{"a":1}"#])
        .assert()
        .success();
    assert_eq!(w.next_line(), "1");
}

/// A value going away is a change like any other: under json, the frame
/// after `put … null` is `null`.
#[test]
fn json_watch_prints_null_when_the_value_goes() {
    let d = TestDaemon::spawn();
    d.comb(&d.home)
        .args(["put", "--global", "my", r#"{"a":1}"#])
        .assert()
        .success();

    let mut w = watch_lines(&d, &d.home, &["--global", "my.a", "-f", "json"]);
    assert_eq!(w.next_line(), "1");

    d.comb(&d.home)
        .args(["put", "--global", "my", "null"])
        .assert()
        .success();
    assert_eq!(w.next_line(), "null");
}

/// `comb get` prints nothing for a miss under text, so a text watch prints
/// no line for one: the value going and coming back shows only the values.
#[test]
fn text_watch_prints_no_line_for_a_miss() {
    let d = TestDaemon::spawn();
    d.comb(&d.home)
        .args(["put", "--global", "my", r#"{"a":1}"#])
        .assert()
        .success();

    let mut w = watch_lines(&d, &d.home, &["--global", "my.a"]);
    assert_eq!(w.next_line(), "1");

    d.comb(&d.home)
        .args(["put", "--global", "my", "null"])
        .assert()
        .success();
    d.comb(&d.home)
        .args(["put", "--global", "my", r#"{"a":2}"#])
        .assert()
        .success();
    assert_eq!(w.next_line(), "2");
}

/// The daemon refusing a watched key reads as its refusing the same `get`:
/// `error: <key>: <the daemon's words>`, exit 2 — the words alone, as the C
/// ABI and every SDK report them.
#[test]
fn a_refused_watch_reads_as_a_refused_get() {
    let d = TestDaemon::spawn();
    let stderr = |command: &str| {
        let out = d
            .comb(&d.home)
            .args([command, "--global", "a:b:c.x"])
            .output()
            .expect("run comb");
        assert_eq!(out.status.code(), Some(2), "{command}");
        String::from_utf8(out.stderr).expect("UTF-8 stderr")
    };

    let watched = stderr("watch");
    assert!(
        watched.starts_with("error: a:b:c.x: invalid facet key: a:b:c"),
        "{watched}"
    );
    // A `get` names a version skew after the refusal, which a build from an
    // uncommitted tree has; the refusal itself is the same line.
    let got = stderr("get");
    assert!(
        got.starts_with(watched.trim_end()),
        "{got} against {watched}"
    );
}
