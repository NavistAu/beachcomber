//! `comb get` is a client of `libbeachcomber::resolve` (design §5.1): the
//! daemon holds templates as data under `<p>:template`, and the binary renders
//! them with its own environment and directory, at any depth, through the one
//! resolver every surface shares. These tests run the built `comb` against a
//! daemon at a temp socket with a temp `$HOME`/`$XDG_CONFIG_HOME`, the §3.1
//! template tree put through `comb put`, and `SCANLINE_FOO=hi` in the shell.
//!
//! Outcomes pinned: 1, 2, 3, 4, 4a, 8, 20, 22, 23 of the design's §12.

use crate::common::daemon::{TestDaemon, isolate};
use crate::common::git::{GitRepoFixture, has_git};
use crate::common::socket::IsolatedSocket;
use std::path::{Path, PathBuf};

/// The §3.1 tree, as `comb put scanline:template` takes it, plus a third
/// level (`widgets.clock.format`) for the flattening outcome.
const TREE: &str = r#"{"theme":"{{ env.SCANLINE_THEME or \"dark\" }}","widgets":{"foo":"{{ env.SCANLINE_FOO }}","bar":"{{ comb.hostname.name | truncate(3) }}","clock":{"format":"{{ '%H:%M' }}"}}}"#;

/// A `comb` at `cwd`, pointed at `d`'s socket, with the daemon's temp
/// `$HOME`/`$XDG_CONFIG_HOME` so nothing reaches the developer's config, and
/// the shell the design's examples assume.
fn comb(d: &TestDaemon, cwd: &Path) -> assert_cmd::Command {
    let mut cmd = d.comb(cwd);
    cmd.env("SCANLINE_FOO", "hi").env_remove("SCANLINE_THEME");
    cmd
}

/// A project directory beneath the daemon's `$HOME`, so a put at the cwd and
/// a put at the global entry are two different slots.
fn project(d: &TestDaemon) -> PathBuf {
    let dir = d.home.join("ws").join("project");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn put(d: &TestDaemon, cwd: &Path, args: &[&str]) {
    comb(d, cwd).arg("put").args(args).assert().success();
}

/// The §3.1 tree at `cwd`, plus the static `scanline = {lit: 1}` beside it.
fn seed_scanline(d: &TestDaemon, cwd: &Path) {
    put(d, cwd, &["scanline:template", TREE]);
    put(d, cwd, &["scanline", r#"{"lit":1}"#]);
}

fn stdout(d: &TestDaemon, cwd: &Path, args: &[&str]) -> String {
    let out = comb(d, cwd).arg("get").args(args).assert().success();
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

fn json(d: &TestDaemon, cwd: &Path, args: &[&str]) -> serde_json::Value {
    serde_json::from_str(&stdout(d, cwd, args)).expect("stdout is JSON")
}

/// What `comb.hostname.name | truncate(3)` renders to on this host.
fn short_host(d: &TestDaemon, cwd: &Path) -> String {
    let name = stdout(d, cwd, &["hostname.name"]);
    assert!(!name.is_empty(), "hostname.name must resolve on this host");
    let mut s: String = name.chars().take(3).collect();
    if name.chars().count() > 3 {
        s.push_str("...");
    }
    s
}

// ── outcomes 1–3: a nested template, a group, a namespace ────────────────────

#[test]
fn a_nested_template_leaf_is_addressable() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    // Bare, no trailing newline: a value embeds in a prompt verbatim.
    assert_eq!(stdout(&d, &cwd, &["scanline.widgets.foo"]), "hi");
}

#[test]
fn a_group_renders_every_leaf_and_keeps_its_shape() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    let bar = short_host(&d, &cwd);
    assert_eq!(
        json(&d, &cwd, &["scanline.widgets", "-f", "json"]),
        serde_json::json!({"bar": bar, "clock": {"format": "%H:%M"}, "foo": "hi"})
    );
}

#[test]
fn a_namespace_merges_the_rendered_tree_over_its_data() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    let bar = short_host(&d, &cwd);
    assert_eq!(
        json(&d, &cwd, &["scanline", "-f", "json"]),
        serde_json::json!({
            "lit": 1,
            "theme": "dark",
            "widgets": {"bar": bar, "clock": {"format": "%H:%M"}, "foo": "hi"}
        })
    );
}

// ── outcome 4a: an address runs past a template into what it renders ─────────

#[test]
fn an_address_past_a_template_walks_into_the_rendered_object() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);
    put(
        &d,
        &cwd,
        &[
            "scanline:template.stats",
            r#""{{ {\"cpu\": 40, \"mem\": 12} }}""#,
        ],
    );

    assert_eq!(stdout(&d, &cwd, &["scanline.stats.cpu"]), "40");

    // A key the rendered object does not hold is a miss: under `text` the
    // empty string and exit 0, under `json` an error that explains itself.
    comb(&d, &cwd)
        .args(["get", "scanline.stats.nope"])
        .assert()
        .success()
        .stdout(predicates::str::is_empty())
        .stderr(predicates::str::is_empty());
    comb(&d, &cwd)
        .args(["get", "-f", "json", "scanline.stats.nope"])
        .assert()
        .code(2)
        .stdout(predicates::str::is_empty())
        .stderr(predicates::str::contains("no data (cache miss)"));
}

#[test]
fn an_address_past_a_template_indexes_a_rendered_array() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);
    put(
        &d,
        &cwd,
        &["scanline:template.list", r#""{{ [\"a\",\"b\"] }}""#],
    );

    assert_eq!(stdout(&d, &cwd, &["scanline.list.0"]), "a");
    // The array itself flattens by decimal index under `text`.
    assert_eq!(stdout(&d, &cwd, &["scanline.list"]), "0=a\n1=b");
}

// ── outcome 4: a template at a cached field's path wins on read ──────────────

#[test]
fn a_template_at_a_cached_fields_path_wins_and_the_raw_template_stays_readable() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let d = TestDaemon::spawn();
    let repo = GitRepoFixture::new();
    let cwd = repo.path().to_path_buf();
    put(&d, &cwd, &["git:template.branch", r#""{{ \"main\" }}""#]);

    assert_eq!(stdout(&d, &cwd, &["git.branch"]), "main");
    assert_eq!(
        stdout(&d, &cwd, &["git:template.branch"]),
        r#"{{ "main" }}"#
    );
}

// ── outcome 8: text and sh flatten at any depth ──────────────────────────────

#[test]
fn text_flattens_every_level_to_sorted_leaf_lines() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    let bar = short_host(&d, &cwd);
    // Three levels deep, one line per leaf, sorted (spec outcome 8: before,
    // `widgets.clock={"format":"%H:%M"}`).
    assert_eq!(
        stdout(&d, &cwd, &["scanline", "-f", "text"]),
        format!("lit=1\ntheme=dark\nwidgets.bar={bar}\nwidgets.clock.format=%H:%M\nwidgets.foo=hi")
    );
}

#[test]
fn sh_shell_quotes_every_flattened_value() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);
    put(&d, &cwd, &["q", r#"{"v":"it's a b"}"#]);

    let bar = short_host(&d, &cwd);
    assert_eq!(
        stdout(&d, &cwd, &["scanline", "-f", "sh"]),
        format!(
            "lit='1'\ntheme='dark'\nwidgets.bar='{bar}'\nwidgets.clock.format='%H:%M'\nwidgets.foo='hi'"
        )
    );
    // A space and a single quote survive: POSIX single-quoting, `'` as `'\''`.
    assert_eq!(stdout(&d, &cwd, &["q", "-f", "sh"]), r"v='it'\''s a b'");
}

// ── outcome 23: a config error in one leaf fails the query, naming it ────────

#[test]
fn a_cycle_fails_the_query_naming_both_nodes() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);
    put(
        &d,
        &cwd,
        &[
            "scanline:template.widgets.a",
            r#""{{ scanline.widgets.b }}""#,
        ],
    );
    put(
        &d,
        &cwd,
        &["scanline:template.widgets.b", r#""{{ scanline.widgets }}""#],
    );

    comb(&d, &cwd)
        .args(["get", "scanline.widgets"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("virtual field cycle detected"))
        .stderr(predicates::str::contains("scanline.widgets"));
}

// ── outcome 22: comb always uses the daemon, even for env.* ──────────────────

/// Stops the daemon `comb get` started on its own, so a test never leaves one
/// behind: `ensure_daemon` forks without `--exit-with-parent`.
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

#[test]
fn env_keys_answer_from_the_shell_and_still_ensure_the_daemon() {
    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    std::fs::create_dir_all(&home).unwrap();
    let guard = SpawnedDaemon {
        socket,
        home: home.clone(),
        xdg: xdg.clone(),
    };
    assert!(
        !guard.socket.path.exists(),
        "no daemon before the first get"
    );

    let comb = || {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
        isolate(&mut cmd, &home, &xdg, &guard.socket.path)
            .env_remove("NOPE")
            .current_dir(&home);
        assert_cmd::Command::from_std(cmd)
    };

    comb()
        .args(["get", "env.HOME"])
        .assert()
        .success()
        .stdout(predicates::str::diff(home.to_string_lossy().into_owned()));
    // Canon invariant 6: an env miss is `""`, never an error and never a miss.
    comb()
        .args(["get", "env.NOPE"])
        .assert()
        .success()
        .stdout(predicates::str::is_empty())
        .stderr(predicates::str::is_empty());

    assert!(
        guard.socket.path.exists(),
        "comb get started the daemon even though env.* needed no request"
    );
    assert!(
        xdg.join("beachcomber").join("defaults").is_dir(),
        "the daemon comb started wrote its managed defaults into the temp config dir"
    );
}

/// A shell may carry a value that is not UTF-8. `comb get` reads its
/// environment lossily: such a value never aborts the command, and asked for
/// directly it is the lossy string.
#[test]
fn a_non_utf8_environment_value_never_panics_and_reads_lossily() {
    use std::os::unix::ffi::OsStrExt;

    let d = TestDaemon::spawn();
    let cwd = project(&d);
    let bad = std::ffi::OsStr::from_bytes(b"ok\xff\xfe");

    comb(&d, &cwd)
        .env("BAD", bad)
        .args(["get", "env.HOME"])
        .assert()
        .success()
        .stdout(predicates::str::diff(d.home.to_string_lossy().into_owned()));
    comb(&d, &cwd)
        .env("BAD", bad)
        .args(["get", "env.BAD"])
        .assert()
        .success()
        .stdout(predicates::str::diff("ok\u{FFFD}\u{FFFD}"));
}

// ── outcome 17 through the real path: a managed default renders ──────────────

#[test]
fn a_managed_default_renders_through_comb_get() {
    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    let defaults = xdg.join("beachcomber").join("defaults");
    beachcomber::daemon::defaults::write_managed_defaults(&defaults, env!("BEACHCOMBER_VERSION"))
        .expect("write managed defaults");
    assert!(defaults.join("conda.toml").is_file());
    let d = TestDaemon::spawn_in(socket, home, xdg);
    let cwd = project(&d);

    let out = comb(&d, &cwd)
        .env("CONDA_DEFAULT_ENV", "base")
        .args(["get", "conda.env"])
        .assert()
        .success();
    assert_eq!(
        String::from_utf8(out.get_output().stdout.clone()).unwrap(),
        "base"
    );
}

// ── outcome 20: kubecontext is a template over kubectl ───────────────────────

/// A daemon started with the managed defaults written, plus a `kubectl` on
/// the child's `PATH` that prints `ctx-1` for `config current-context` and
/// nothing for `config view …`.
fn daemon_with_defaults_and_fake_kubectl() -> (TestDaemon, PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    beachcomber::daemon::defaults::write_managed_defaults(
        &xdg.join("beachcomber").join("defaults"),
        env!("BEACHCOMBER_VERSION"),
    )
    .expect("write managed defaults");
    let bin = socket.dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let kubectl = bin.join("kubectl");
    std::fs::write(
        &kubectl,
        "#!/bin/sh\ncase \"$2\" in current-context) printf 'ctx-1' ;; esac\n",
    )
    .unwrap();
    std::fs::set_permissions(&kubectl, std::fs::Permissions::from_mode(0o755)).unwrap();
    (TestDaemon::spawn_in(socket, home, xdg), bin)
}

fn path_with(bin: &Path) -> String {
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// `kubecontext.context` is `defaults/kubecontext.toml`'s template — the same
/// field name the deleted daemon provider served, rendered through `exec()`
/// over the `kubectl` the shell would run.
#[test]
fn kubecontext_context_renders_through_kubectl() {
    let (d, bin) = daemon_with_defaults_and_fake_kubectl();
    let cwd = project(&d);

    let out = comb(&d, &cwd)
        .env("PATH", path_with(&bin))
        .args(["get", "kubecontext.context"])
        .assert()
        .success();
    assert_eq!(
        String::from_utf8(out.get_output().stdout.clone()).unwrap(),
        "ctx-1"
    );
}

/// `kubecontext.namespace` falls back to `default` when `kubectl config view`
/// prints no namespace.
#[test]
fn kubecontext_namespace_defaults_when_kubectl_prints_none() {
    let (d, bin) = daemon_with_defaults_and_fake_kubectl();
    let cwd = project(&d);

    let out = comb(&d, &cwd)
        .env("PATH", path_with(&bin))
        .args(["get", "kubecontext.namespace"])
        .assert()
        .success();
    assert_eq!(
        String::from_utf8(out.get_output().stdout.clone()).unwrap(),
        "default"
    );
}

// ── templates resolve from the requested slot ────────────────────────────────

#[test]
fn a_template_put_at_the_cwd_is_not_visible_to_a_global_read() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    assert_eq!(stdout(&d, &cwd, &["scanline.widgets.foo"]), "hi");
    comb(&d, &cwd)
        .args(["get", "--global", "scanline.widgets.foo"])
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

#[test]
fn a_template_put_globally_is_visible_to_both_reads() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(&d, &cwd, &["--global", "scanline:template", TREE]);

    assert_eq!(stdout(&d, &cwd, &["scanline.widgets.foo"]), "hi");
    assert_eq!(
        stdout(&d, &cwd, &["--global", "scanline.widgets.foo"]),
        "hi"
    );
}

// ── a key the daemon rejects is an error, in the daemon's words ──────────────

/// The daemon answers `ok: false` for an invalid facet key. The key the user
/// typed being refused is an error in every format, worded by the daemon —
/// only a ref inside a template naming such a key is falsy (canon: a missing
/// ref is falsy at any depth).
#[test]
fn a_daemon_rejected_key_is_an_error_in_every_format() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);

    for format in ["text", "json"] {
        comb(&d, &cwd)
            .args(["get", "-f", format, "a:b:c.x"])
            .assert()
            .code(2)
            .stdout(predicates::str::is_empty())
            .stderr(predicates::str::contains("Error querying a:b:c.x: "))
            .stderr(predicates::str::contains("invalid facet key"));
    }
}

/// An unknown provider is a miss, not a rejected key: silent exit 0 under
/// `text`, exit 2 under `json`.
#[test]
fn an_unknown_provider_is_a_miss() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);

    comb(&d, &cwd)
        .args(["get", "nope.x"])
        .assert()
        .code(0)
        .stdout(predicates::str::is_empty())
        .stderr(predicates::str::is_empty());
    comb(&d, &cwd)
        .args(["get", "-f", "json", "nope.x"])
        .assert()
        .code(2)
        .stdout(predicates::str::is_empty());
}

#[test]
fn a_rejected_ref_inside_a_template_is_falsy() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(
        &d,
        &cwd,
        &["t:template.v", r#""{{ comb.nope.x or 'fb' }}""#],
    );

    assert_eq!(stdout(&d, &cwd, &["t.v"]), "fb");
}

// ── multi-key: every key resolves independently ──────────────────────────────

#[test]
fn multi_key_output_keeps_its_shape_per_format_and_a_miss_reports_itself() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    // text: one rendering per key, a miss prints nothing, exit 0.
    assert_eq!(
        stdout(
            &d,
            &cwd,
            &["scanline.lit", "scanline.nope", "scanline.widgets.foo"]
        ),
        "1\nhi\n"
    );
    // json: an array of resolved values in key order.
    assert_eq!(
        json(
            &d,
            &cwd,
            &["-f", "json", "scanline.lit", "scanline.widgets.foo"]
        ),
        serde_json::json!([1, "hi"])
    );
    // CSV: an object contributes its sorted fields, a scalar its key.
    assert_eq!(
        stdout(
            &d,
            &cwd,
            &["-f", "CSV", "scanline.lit", "scanline.widgets.clock"]
        ),
        "scanline.lit,format\n1,%H:%M\n"
    );
    // A miss under a structured format is an error for that key alone and
    // the others still render — with the document keeping one slot per key:
    // `null` in the array, an empty cell in the row.
    let out = comb(&d, &cwd)
        .args([
            "get",
            "-f",
            "json",
            "scanline.lit",
            "scanline.nope",
            "scanline.widgets.foo",
        ])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "Error querying scanline.nope: no data (cache miss)",
        ));
    let parsed: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(parsed, serde_json::json!([1, null, "hi"]));

    let out = comb(&d, &cwd)
        .args([
            "get",
            "-f",
            "CSV",
            "scanline.lit",
            "scanline.nope",
            "scanline.widgets.foo",
        ])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "Error querying scanline.nope: no data (cache miss)",
        ));
    assert_eq!(
        String::from_utf8(out.get_output().stdout.clone()).unwrap(),
        "scanline.lit,scanline.nope,scanline.widgets.foo\n1,,hi\n"
    );
}
