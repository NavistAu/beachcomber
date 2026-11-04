//! End-to-end tests for `comb eval`: it is a client of
//! `libbeachcomber::resolve`, exactly as `comb get` is, so every ref inside
//! its template — `env.*`, a plain `provider.field`, a `comb.*` ref, a
//! nested template field — resolves through the one resolver. `comb` always
//! requires the daemon (design §2, outcome 22): there is no source that
//! skips starting one, `env.*`-only included.
//!
//! Reference discovery is `libbeachcomber::eval::discover_refs`, covered in
//! `libbeachcomber/tests/suite/eval_classify.rs` and `tests/suite/get_template.rs`. The
//! bulk of `eval`'s render-contract tests (literal text, typed single-tag
//! results, the `default` filter on a miss, …) live in `tests/suite/cli_golden.rs`.

use crate::common::daemon::{TestDaemon, isolate};
use crate::common::socket::IsolatedSocket;
use std::path::{Path, PathBuf};

// ── outcome 22: `comb eval` always starts the daemon ─────────────────────────

/// Stops the daemon `comb eval` started on its own, so a test never leaves
/// one behind: `ensure_daemon` forks without `--exit-with-parent`. `kill`
/// gets the same temp `$HOME`/`$XDG_CONFIG_HOME` as the `eval` that started
/// the daemon, like every other `comb` spawn in this file — the developer's
/// real config dir must never be touched by a test.
struct SpawnedDaemon {
    socket: IsolatedSocket,
    home: PathBuf,
    xdg: PathBuf,
}

impl Drop for SpawnedDaemon {
    fn drop(&mut self) {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
        let _ = isolate(&mut cmd, &self.home, &self.xdg, &self.socket.path)
            .args(["kill", "--socket"])
            .arg(&self.socket.path)
            .output();
    }
}

/// A source needing only `env.*` still starts the daemon: `comb` always
/// requires one (design §2), no source is exempt. Before this task, an
/// empty daemon-ref closure skipped `ensure_daemon` entirely.
#[test]
fn eval_starts_the_daemon() {
    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    std::fs::create_dir_all(&home).unwrap();
    let guard = SpawnedDaemon {
        socket,
        home: home.clone(),
        xdg: xdg.clone(),
    };
    assert!(!guard.socket.path.exists(), "no daemon before eval runs");

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
    isolate(&mut cmd, &home, &xdg, &guard.socket.path)
        .env("FOO", "hi")
        .args(["eval", "{{ env.FOO }}"]);
    assert_cmd::Command::from_std(cmd)
        .assert()
        .success()
        .stdout(predicates::str::diff("hi"));

    assert!(
        guard.socket.path.exists(),
        "comb eval must start the daemon (outcome 22)"
    );
}

// ── refs resolve through the same daemon-served template store `get` uses ────

/// `aws.profile` is a managed default (`defaults/aws.toml`, seeded by every
/// daemon on start) — a template served under `aws:template`, not a
/// hand-declared field. `eval` fetches it from the daemon and renders it with
/// the calling shell's environment, same as `comb get aws.profile` would.
#[test]
fn eval_resolves_daemon_served_template_from_env() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .env("AWS_PROFILE", "prod-account")
        .args(["eval", "{{ aws.profile }}"])
        .assert()
        .success()
        .stdout(predicates::str::contains("prod-account"));
}

/// The addresses `comb get` takes are the ones a template writes: a quoted
/// segment, a facet head in brackets, a computed provider name, a subscript
/// of the caller's environment.
#[test]
fn eval_reads_the_same_addresses_get_takes() {
    let d = TestDaemon::spawn();
    d.comb(Path::new("/"))
        .args(["put", "--global", "p", r#"{"cargo:x":"1.0","o":{"a b":2}}"#])
        .assert()
        .success();

    for (src, want) in [
        ("{{ comb.p['cargo:x'] }}", "1.0"),
        ("{{ comb['p'].o['a b'] }}", "2"),
        ("{{ comb[('p')]['cargo:x'] }}", "1.0"),
        ("{{ comb['p:put'].o['a b'] }}", "2"),
        ("{{ p['o']['a b'] }}", "2"),
        ("{{ env[('PICK')] }}", "picked"),
    ] {
        d.comb(Path::new("/"))
            .env("PICK", "picked")
            .args(["eval", src])
            .assert()
            .success()
            .stdout(predicates::str::diff(want));
    }
}

/// A source that does not compile is the caller's to fix: `1 +` is not a
/// valid minijinja expression, so `eval` exits 2 naming the failure as a
/// compile error rather than trying to render anything.
#[test]
fn eval_compile_error_exits_2_naming_it() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args(["eval", "{{ 1 + }}"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("compile error"));
}

/// A macro defined in the text it is called from renders on the client, and
/// a call block hands it a body; `include` is refused before anything renders.
#[test]
fn eval_renders_an_inline_macro() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args([
            "eval",
            "{% macro tag(x) %}<{{ x }}{{ caller() if caller }}>{% endmacro %}\
             {{ tag('a') }}{% call tag('b') %}!{% endcall %}",
        ])
        .assert()
        .success()
        .stdout(predicates::str::diff("<a><b!>"));

    d.comb(Path::new("/"))
        .args(["eval", "{% include 'p:macro.x' %}"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "{% include %} is refused: text is reused with a macro",
        ));
}

/// `comb eval` and a `-f` format import a library as a stored template does:
/// the same library, read through the same resolver.
#[test]
fn eval_and_a_format_import() {
    let d = TestDaemon::spawn();
    d.comb(Path::new("/"))
        .args([
            "put",
            "--global",
            "fx:macro.wrap",
            r#""{% macro wrap(x) %}[{{ x }}]{% endmacro %}""#,
        ])
        .assert()
        .success();
    d.comb(Path::new("/"))
        .args(["put", "--global", "fx", r#"{"v":"ok"}"#])
        .assert()
        .success();

    d.comb(Path::new("/"))
        .args([
            "eval",
            "{% from 'fx:macro.wrap' import wrap %}{{ wrap(fx.v) }}",
        ])
        .assert()
        .success()
        .stdout(predicates::str::diff("[ok]"));
    d.comb(Path::new("/"))
        .args([
            "get",
            "-f",
            "{% import 'fx:macro.wrap' as w %}{{ w.wrap(value) }}",
            "fx.v",
        ])
        .assert()
        .success()
        .stdout(predicates::str::diff("[ok]"));
    d.comb(Path::new("/"))
        .args([
            "eval",
            "{% from 'fx:macro.nope' import wrap %}{{ wrap(1) }}",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "fx:macro.nope: no macro library there",
        ));
}

/// `url()` and `plugin()` run only in a Source's lines: on the client they are
/// refused by name, not reported as unknown functions.
#[test]
fn url_on_the_client_is_refused_clearly() {
    let d = TestDaemon::spawn();

    for call in ["url('http://127.0.0.1:1/')", "plugin('a', 'b')"] {
        let name = call.split('(').next().unwrap();
        d.comb(Path::new("/"))
            .args(["eval", &format!("{{{{ {call} }}}}")])
            .assert()
            .failure()
            .stderr(predicates::str::contains(format!(
                "{name}() runs only in a Source's lines"
            )));
    }
}

// ── `-f`: the value formats `get` and `watch` take ───────────────────────────

/// `eval -f json` keeps a number a number, where text cannot tell it from a
/// string.
#[test]
fn eval_f_json_keeps_a_number() {
    let d = TestDaemon::spawn();
    d.comb(Path::new("/"))
        .args(["eval", "-f", "json", "{{ 1 + 1 }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("2\n"));
    d.comb(Path::new("/"))
        .args(["eval", "-f", "json", "n={{ 1 + 1 }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("\"n=2\"\n"));
}

/// An absent value is `null` under json, as `get -f json` prints one, and
/// nothing under text.
#[test]
fn eval_f_json_of_an_absent_value_is_null() {
    let d = TestDaemon::spawn();
    d.comb(Path::new("/"))
        .args(["eval", "-f", "json", "{{ none }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("null\n"));
}

/// `eval -f sh` prints shell assignments for an object's fields, as `get`
/// does; the `e.j` suffix spells json.
#[test]
fn eval_f_sh_and_the_json_suffix() {
    let d = TestDaemon::spawn();
    d.comb(Path::new("/"))
        .args(["eval", "-f", "sh", "{{ {'a': 'x y'} }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("a='x y'\n"));
    d.comb(Path::new("/"))
        .args(["e.j", "{{ 3 }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff("3\n"));
}

/// A format `eval` has no use for is refused, naming the ones it has.
#[test]
fn eval_refuses_a_format_it_does_not_have() {
    let d = TestDaemon::spawn();
    d.comb(Path::new("/"))
        .args(["eval", "-f", "csv", "{{ 1 }}"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("eval has no 'csv' format"));
}
