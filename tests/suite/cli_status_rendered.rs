//! `comb status` shows the caller's *rendered* template leaves beside the
//! cached rows, and every row's SOURCE.
//!
//! The daemon holds templates as data under `<p>:template` and never
//! evaluates them (canon `field_resolution.md` invariant 7). `status` is a
//! surface that may `exec()` (canon `field_resolution.md` invariant 15), so
//! the CLI resolves each template leaf for its own directory and environment
//! — exactly as `comb get` would — and adds one marked row per leaf. Those
//! rows carry `source = template` and `kind = template`; nothing on the wire
//! ever does.
//!
//! The fixture is the design's §3.1 tree, as `tests/suite/cli_resolve.rs`
//! puts it, with `SCANLINE_FOO=hi` in the shell.

use crate::common::daemon::TestDaemon;
use crate::common::git::{GitRepoFixture, has_git};
use crate::common::poll::poll_until;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The §3.1 tree, as `comb put scanline:template` takes it.
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

fn status(d: &TestDaemon, cwd: &Path, args: &[&str]) -> String {
    let out = comb(d, cwd).arg("status").args(args).assert().success();
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

/// Every run of whitespace collapsed to one space, so a column-aligned table
/// can be asserted on without pinning padding widths.
fn squashed(out: &str) -> Vec<String> {
    out.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

fn assert_row(out: &str, prefix: &str) {
    assert!(
        squashed(out).iter().any(|l| l.starts_with(prefix)),
        "expected a row starting {prefix:?} in:\n{out}"
    );
}

/// `comb status -f json` rows, one per NDJSON line.
fn json_rows(d: &TestDaemon, cwd: &Path, args: &[&str]) -> Vec<serde_json::Value> {
    let mut all = vec!["-f", "json"];
    all.extend_from_slice(args);
    status(d, cwd, &all)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("status -f json emits NDJSON"))
        .collect()
}

// ── the rendered rows, and the SOURCE column ────────────────────────────────

#[test]
fn rendered_leaves_sit_beside_the_raw_templates_and_the_cached_data() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    let out = status(
        &d,
        &cwd,
        &["--all", "--color", "never", "--ascii", "--no-trunc"],
    );

    // The rendered leaves: SOURCE `template`, VALUE marked with the glyph
    // that stands in for the cyan the colour build uses.
    assert_row(&out, "scanline widgets.foo rendered > hi ");
    assert_row(&out, "scanline theme rendered > dark ");
    // A depth-2 leaf renders too, not just the top-level ones.
    assert_row(&out, "scanline widgets.clock.format rendered > %H:%M ");
    // The template itself is still data the daemon holds, put by the user.
    assert_row(
        &out,
        "scanline:template widgets.foo put {{ env.SCANLINE_FOO }} ",
    );
    // And a plain static record is unmarked, with its own SOURCE.
    assert_row(&out, "scanline lit put 1 ");
}

/// Facet rows are data about a provider, not values a read answers: hidden
/// unless `--all` or a `facet` filter asks for them.
#[test]
fn facet_rows_are_hidden_unless_asked_for() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    let shown = json_rows(&d, &cwd, &[]);
    assert!(
        shown
            .iter()
            .all(|r| !r["provider"].as_str().unwrap().contains(':')),
        "no facet row by default: {shown:#?}"
    );
    assert!(
        shown
            .iter()
            .any(|r| r["provider"] == "scanline" && r["field"] == "widgets.foo"),
        "the rendered value is still there, from the hidden template: {shown:#?}"
    );

    let all = json_rows(&d, &cwd, &["--all"]);
    assert!(
        all.iter().any(|r| r["provider"] == "scanline:template"),
        "--all shows the facet rows: {all:#?}"
    );

    let templates = json_rows(&d, &cwd, &["--filter", "facet=template"]);
    assert!(!templates.is_empty());
    assert!(
        templates
            .iter()
            .all(|r| r["provider"].as_str().unwrap().ends_with(":template")),
        "facet=template is the template records alone: {templates:#?}"
    );
}

/// A leaf that renders nothing is a null row: hidden by the default
/// `value!=-`, shown by `--filter value=-` or `--all`.
#[test]
fn an_absent_leaf_is_a_null_row_shown_only_when_asked() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(&d, &cwd, &["t:template.nothing", r#""{{ none }}""#]);
    put(&d, &cwd, &["t:template.some", r#""{{ 'x' }}""#]);

    let shown = json_rows(&d, &cwd, &["--filter", "provider=t"]);
    assert!(
        !shown.iter().any(|r| r["field"] == "nothing"),
        "an absent leaf is hidden by default: {shown:#?}"
    );
    assert!(shown.iter().any(|r| r["field"] == "some"));

    let absent = json_rows(&d, &cwd, &["--filter", "provider=t", "--filter", "value=-"]);
    assert_eq!(absent.len(), 1, "only the absent leaf: {absent:#?}");
    assert_eq!(absent[0]["field"], "nothing");
    assert!(absent[0]["value"].is_null());
    assert_eq!(absent[0]["kind"]["kind"], "rendered");

    let out = status(
        &d,
        &cwd,
        &[
            "-a",
            "--filter",
            "provider=t",
            "--color",
            "never",
            "--ascii",
            "--no-trunc",
        ],
    );
    assert_row(&out, "t nothing rendered > null ");
}

#[test]
fn the_human_header_names_source_after_field() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    let out = status(&d, &cwd, &["--color", "never", "--ascii", "--no-trunc"]);
    assert_eq!(
        squashed(&out).first().map(String::as_str),
        Some("PROVIDER FIELD SOURCE VALUE PATH AGE POLL TTL"),
        "in:\n{out}"
    );
}

#[test]
fn colour_marks_the_rendered_value_cyan_rather_than_with_a_glyph() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    let out = status(&d, &cwd, &["--color", "always", "--no-trunc"]);
    assert!(
        out.contains("\x1b[36mhi\x1b[0m"),
        "rendered VALUE should be cyan under --color always:\n{out}"
    );
    assert!(
        !out.contains('\u{25b8}'),
        "the glyph stands in for colour, never beside it:\n{out}"
    );
}

#[test]
fn colour_off_with_no_ascii_marks_the_rendered_value_with_the_unicode_glyph() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    // Default glyph set (no --ascii), colour off: the Unicode marker prefixes
    // the rendered VALUE.
    let out = status(&d, &cwd, &["--color", "never", "--no-trunc"]);
    assert_row(&out, "scanline widgets.foo rendered \u{25b8} hi ");
}

/// Rendering `widgets.bar` reads `comb.hostname.name`, which runs the
/// hostname Source on a cold daemon. The table is read after the render, so
/// the record that render produced is a row of the same `status`, not only of
/// the next one.
#[test]
fn what_a_render_runs_is_a_row_of_the_same_status() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    let rows = json_rows(&d, &cwd, &[]);
    assert!(
        rows.iter()
            .any(|r| r["provider"] == "hostname" && r["field"] == "name"),
        "the hostname record the render ran is a row: {rows:#?}"
    );
}

#[test]
fn rendered_rows_carry_kind_and_source_rendered_in_json() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    let rows = json_rows(&d, &cwd, &[]);
    let rendered: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|r| r["provider"] == "scanline" && r["field"] == "widgets.foo")
        .collect();
    assert_eq!(rendered.len(), 1, "one rendered widgets.foo row: {rows:#?}");
    assert_eq!(rendered[0]["source"], "rendered");
    assert_eq!(rendered[0]["kind"]["kind"], "rendered");
    assert_eq!(rendered[0]["value"], "hi");
}

#[test]
fn the_lifecycle_filter_selects_only_the_rendered_rows() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    let rows = json_rows(&d, &cwd, &["--filter", "lifecycle=rendered"]);
    assert!(!rows.is_empty(), "the fixture has rendered rows");
    for row in &rows {
        assert_eq!(row["source"], "rendered", "in {row}");
        assert_eq!(row["kind"]["kind"], "rendered", "in {row}");
    }
    let fields: Vec<&str> = rows.iter().filter_map(|r| r["field"].as_str()).collect();
    assert!(fields.contains(&"theme"), "got {fields:?}");
    assert!(fields.contains(&"widgets.foo"), "got {fields:?}");
}

#[test]
fn a_leaf_that_fails_shows_its_error_text_as_the_value() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(
        &d,
        &cwd,
        &["boom:template.leaf", r#""{{ exec(['false']) }}""#],
    );

    let rows = json_rows(&d, &cwd, &["--filter", "provider=boom"]);
    let rendered: Vec<&serde_json::Value> =
        rows.iter().filter(|r| r["source"] == "rendered").collect();
    assert_eq!(rendered.len(), 1, "one rendered leaf: {rows:#?}");
    let value = rendered[0]["value"]
        .as_str()
        .expect("error text is a string");
    assert!(
        value.contains("`false`") && value.contains("exit status"),
        "the failure should be the cell's text: {value:?}"
    );
}

#[test]
fn a_string_form_leaf_that_fails_names_the_script() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(
        &d,
        &cwd,
        &["boom:template.leaf", r#""{{ exec('exit 3') }}""#],
    );

    let rows = json_rows(&d, &cwd, &["--filter", "provider=boom"]);
    let rendered: Vec<&serde_json::Value> =
        rows.iter().filter(|r| r["source"] == "rendered").collect();
    assert_eq!(rendered.len(), 1, "one rendered leaf: {rows:#?}");
    let value = rendered[0]["value"]
        .as_str()
        .expect("error text is a string");
    assert!(
        value.contains("sh -c 'exit 3'") && value.contains("exit status"),
        "the failure should name the script: {value:?}"
    );
}

#[test]
fn a_template_put_at_another_path_is_not_rendered_here() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    let other = d.home.join("ws").join("elsewhere");
    std::fs::create_dir_all(&other).unwrap();

    seed_scanline(&d, &cwd);
    put(
        &d,
        &other,
        &["offstage:template.leaf", r#""{{ \"nope\" }}""#],
    );

    let rows = json_rows(&d, &cwd, &["-a"]);
    // The template is data, so its raw row is still listed…
    assert!(
        rows.iter().any(|r| r["provider"] == "offstage:template"),
        "the raw template row belongs to the other path: {rows:#?}"
    );
    // …but nothing renders it for this directory.
    assert!(
        !rows
            .iter()
            .any(|r| r["provider"] == "offstage" && r["source"] == "rendered"),
        "a template at another path must not be rendered here: {rows:#?}"
    );
}

/// A template over a cached field answers its read, so the default view shows
/// the rendered row alone; the cached record it shadows is a row under `-a`,
/// marked `shadowed` in json.
#[test]
fn a_cached_field_with_a_template_over_it_is_shadowed_by_it() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let d = TestDaemon::spawn();
    let repo = GitRepoFixture::new();
    let cwd = repo.path().to_path_buf();

    // Warm the git source at this repo, so `git.branch` really is cached.
    comb(&d, &cwd)
        .args(["get", "git.branch", "--wait"])
        .assert()
        .success();
    poll_until(
        "comb status rows for the warmed git repo",
        "a cached git.branch row",
        Duration::from_secs(5),
        Duration::from_millis(50),
        || json_rows(&d, &cwd, &["--filter", "provider=git"]),
        |rows| {
            rows.iter()
                .any(|r| r["field"] == "branch" && r["source"] != "rendered")
        },
    );

    put(
        &d,
        &cwd,
        &["git:template.branch", r#""{{ \"overridden\" }}""#],
    );

    let rows = json_rows(
        &d,
        &cwd,
        &["--filter", "provider=git", "--filter", "field=branch"],
    );
    assert_eq!(rows.len(), 1, "the rendered row alone: {rows:#?}");
    assert_eq!(rows[0]["source"], "rendered");
    assert_eq!(rows[0]["value"], "overridden");
    assert_eq!(rows[0]["kind"]["kind"], "rendered");

    let rows = json_rows(
        &d,
        &cwd,
        &[
            "-a",
            "--filter",
            "provider=git",
            "--filter",
            "field=branch",
            "--filter",
            "facet=-",
        ],
    );
    let cached: Vec<&serde_json::Value> =
        rows.iter().filter(|r| r["source"] != "rendered").collect();
    assert_eq!(cached.len(), 1, "the cached row under -a: {rows:#?}");
    assert_eq!(cached[0]["shadowed"], true, "{rows:#?}");
}

#[test]
fn a_template_put_at_the_global_entry_renders_at_a_cwd_below_home() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(
        &d,
        &cwd,
        &["--global", "globalscan:template.leaf", r#""{{ \"hi\" }}""#],
    );

    let rows = json_rows(&d, &cwd, &["--filter", "provider=globalscan"]);
    let rendered: Vec<&serde_json::Value> =
        rows.iter().filter(|r| r["source"] == "rendered").collect();
    assert_eq!(
        rendered.len(),
        1,
        "a global template must render at a cwd below $HOME: {rows:#?}"
    );
    assert_eq!(rendered[0]["value"], "hi");
}

// ── --tree ───────────────────────────────────────────────────────────────────

/// Under each value, `--tree` lists the layers a read from here consults,
/// nearest first: the template record over the data beneath it, a nearer put
/// over a global one. The one the read takes is drawn plainly; the rest say
/// `shadowed` when colour is off. Each leaf is one row: the records it
/// shadows are its children, not rows of their own.
#[test]
fn tree_lists_the_layers_under_each_value_and_marks_the_shadowed_ones() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(&d, &cwd, &["t:template.greet", r#""{{ 'tpl' }}""#]);
    put(&d, &cwd, &["t.greet", r#""data""#]);
    put(&d, &cwd, &["--global", "t.other", r#""far""#]);
    put(&d, &cwd, &["t.other", r#""near""#]);

    let out = status(
        &d,
        &cwd,
        &[
            "--tree",
            "--filter",
            "provider=t",
            "--color",
            "never",
            "--ascii",
            "--no-trunc",
        ],
    );
    let lines = squashed(&out);
    let at = |prefix: &str| {
        lines
            .iter()
            .position(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no line starting {prefix:?} in:\n{out}"))
    };
    let greet = at("t greet rendered > tpl ");
    assert!(
        lines[greet + 1].starts_with("|- :template put {{ 'tpl' }} "),
        "{out}"
    );
    assert!(
        lines[greet + 2].starts_with("`- shadowed put data "),
        "{out}"
    );
    // The near put is the row; only what it shadows hangs under it.
    let near = at("t other put near ");
    assert!(
        lines[near + 1].starts_with("`- shadowed put far ~ "),
        "{out}"
    );
    let top_level = |field: &str| {
        lines
            .iter()
            .filter(|l| l.starts_with(&format!("t {field} ")))
            .count()
    };
    assert_eq!(top_level("greet"), 1, "one greet row:\n{out}");
    assert_eq!(top_level("other"), 1, "one other row:\n{out}");
}

/// `aws.profile` set by the shipped default, by `config.toml` and by a
/// drop-in: under the rendered value, `--tree` lists the drop-in's template,
/// then the two it overrode as shadowed, last file first.
#[test]
fn tree_lists_every_config_file_that_sets_a_template() {
    let socket = crate::common::socket::IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    let dir = xdg.join("beachcomber");
    std::fs::create_dir_all(dir.join("conf.d")).unwrap();
    beachcomber::daemon::defaults::write_managed_defaults(
        &dir.join("defaults"),
        env!("BEACHCOMBER_VERSION"),
    )
    .unwrap();
    std::fs::write(
        dir.join("config.toml"),
        "[client.providers.aws]\ntemplate.profile = \"{{ 'main' }}\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("conf.d").join("10-mine.toml"),
        "[client.providers.aws]\ntemplate.profile = \"{{ 'mine' }}\"\n",
    )
    .unwrap();
    let d = TestDaemon::spawn_in(socket, home, xdg);
    let cwd = project(&d);

    let out = status(
        &d,
        &cwd,
        &[
            "--tree",
            "--filter",
            "provider=aws",
            "--filter",
            "field=profile",
            "--color",
            "never",
            "--ascii",
            "--no-trunc",
        ],
    );
    let lines = squashed(&out);
    let at = lines
        .iter()
        .position(|l| l.starts_with("aws profile rendered > mine "))
        .unwrap_or_else(|| panic!("the rendered row in:\n{out}"));
    let file = |rel: &str| format!("config({})", dir.join(rel).display());
    assert!(
        lines[at + 1].starts_with(&format!(
            "|- :template {} {{{{ 'mine' }}}} ",
            file("conf.d/10-mine.toml")
        )),
        "{out}"
    );
    assert!(
        lines[at + 2].starts_with(&format!(
            "|- :template shadowed {} {{{{ 'main' }}}} ",
            file("config.toml")
        )),
        "{out}"
    );
    assert!(
        lines[at + 3].starts_with(&format!(
            "`- :template shadowed {} ",
            file("defaults/aws.toml")
        )),
        "{out}"
    );
}

/// A cascading Source answers from the project's instance and stores a record
/// at `~` too: `--tree` keeps the project's record, which answered, as the
/// one row, and lists the `~` record under it as shadowed.
#[test]
fn tree_keeps_the_instance_that_answered_as_the_row() {
    let socket = crate::common::socket::IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    let project_dir = home.join("ws").join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(xdg.join("beachcomber")).unwrap();
    std::fs::write(home.join(".mk"), "home\n").unwrap();
    std::fs::write(project_dir.join(".mk"), "proj\n").unwrap();
    std::fs::write(
        xdg.join("beachcomber").join("config.toml"),
        r#"
[daemon.providers.walk.sources.main]
root = "{{ lup('.mk') | dirname }}"
data = "{{ '.mk' | read | trim }}"
result.v = "{{ data }}"
"#,
    )
    .unwrap();
    let d = TestDaemon::spawn_in(socket, home, xdg);
    let cwd = std::fs::canonicalize(&project_dir).unwrap();
    // Read from the project, then from `~`, so both instances hold a record.
    comb(&d, &cwd).args(["get", "walk.v"]).assert().success();
    comb(&d, &cwd)
        .args(["get", "--global", "walk.v"])
        .assert()
        .success();

    let out = status(
        &d,
        &cwd,
        &[
            "--tree",
            "--filter",
            "provider=walk",
            "--color",
            "never",
            "--ascii",
            "--no-trunc",
        ],
    );
    let lines = squashed(&out);
    let rows: Vec<&String> = lines.iter().filter(|l| l.starts_with("walk v ")).collect();
    assert_eq!(rows.len(), 1, "one walk.v row:\n{out}");
    assert!(rows[0].starts_with("walk v source(main) proj "), "{out}");
    let at = lines.iter().position(|l| l == rows[0]).unwrap();
    assert!(
        lines[at + 1].starts_with("`- shadowed source(main) home ~ "),
        "{out}"
    );
}

/// A rendered row is as old as the oldest cached value it read, and has no
/// age when it read none.
#[test]
fn a_rendered_row_is_as_old_as_its_oldest_input() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(&d, &cwd, &["seed", r#"{"v":"x"}"#]);
    put(&d, &cwd, &["t:template.reads", r#""{{ comb.seed.v }}""#]);
    put(
        &d,
        &cwd,
        &["t:template.shell", r#""{{ env.SCANLINE_FOO }}""#],
    );

    let rows = json_rows(&d, &cwd, &["--filter", "provider=t"]);
    let row = |field: &str| {
        rows.iter()
            .find(|r| r["field"] == field && r["source"] == "rendered")
            .unwrap_or_else(|| panic!("a rendered {field} row: {rows:#?}"))
            .clone()
    };
    // A put record's age is whatever the clock says by now; the rendered row
    // carries one because its input does.
    let reads = row("reads");
    assert!(reads["age_ms"].is_u64(), "as old as seed.v: {reads}");
    let shell = row("shell");
    assert!(shell.get("age_ms").is_none(), "no cached input: {shell}");

    let human = status(
        &d,
        &cwd,
        &["--filter", "provider=t", "--color", "never", "--ascii"],
    );
    let line = squashed(&human)
        .into_iter()
        .find(|l| l.starts_with("t shell rendered > hi "))
        .unwrap_or_else(|| panic!("a shell row in:\n{human}"));
    assert!(line.ends_with(" - - ---"), "AGE is -: {line:?}");
}

/// In json each row carries its `layers`, the answering one marked.
#[test]
fn tree_json_rows_carry_their_layers() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(&d, &cwd, &["t:template.greet", r#""{{ 'tpl' }}""#]);
    put(&d, &cwd, &["t.greet", r#""data""#]);

    let rows = json_rows(&d, &cwd, &["--tree", "--filter", "provider=t"]);
    let rendered = rows
        .iter()
        .find(|r| r["source"] == "rendered")
        .unwrap_or_else(|| panic!("a rendered row: {rows:#?}"));
    let layers = rendered["layers"].as_array().expect("layers");
    assert_eq!(layers.len(), 2, "{layers:#?}");
    assert_eq!(layers[0]["facet"], "template");
    assert_eq!(layers[0]["answered"], true);
    assert_eq!(layers[1]["label"], "put");
    assert_eq!(layers[1]["value"], "data");
    assert!(layers[1].get("answered").is_none(), "{layers:#?}");
}

/// A row rendered here says so in its SOURCE and its lifecycle, as
/// `rendered`; "template" is the facet and the leaf it was rendered from.
#[test]
fn a_rendered_row_is_rendered_in_every_column_that_names_it() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    let out = comb(&d, &cwd)
        .args(["status", "-f", "sh", "--filter", "lifecycle=rendered"])
        .assert()
        .success();
    let sh = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(sh.contains("_SOURCE='rendered'\n"), "{sh}");
    assert!(sh.contains("_KIND='rendered'\n"), "{sh}");
    assert!(!sh.contains("'template'"), "{sh}");

    comb(&d, &cwd)
        .args(["status", "--filter", "lifecycle=template"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "error: --filter: lifecycle= must be active|decay<N>|once|static|rendered, got template",
        ));
}

#[test]
fn tree_with_another_format_is_a_usage_error() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    comb(&d, &cwd)
        .args(["status", "--tree", "-f", "csv"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "error: --tree: works with the human, table and json formats",
        ));
}

// ── the default view is what a read answers ──────────────────────────────────

/// Rows of `status --filter provider=<p>`, squashed.
fn rows_of(d: &TestDaemon, cwd: &Path, p: &str, extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        "--filter".to_string(),
        format!("provider={p}"),
        "--color".into(),
        "never".into(),
        "--ascii".into(),
        "--no-trunc".into(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = status(d, cwd, &args);
    squashed(&out)
        .into_iter()
        .filter(|l| l.starts_with(&format!("{p} ")))
        .collect()
}

/// By default `status` lists, for the caller's directory, the record each
/// leaf's read takes — a nearer put over a global one, a template over the
/// data beneath it — and none of the records they shadow, as `comb get`
/// answers. `-a` lists them too, and `--filter shadowed=true` lists only them.
#[test]
fn the_default_status_lists_what_a_read_answers() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(&d, &cwd, &["--global", "t.v", r#""far""#]);
    put(&d, &cwd, &["t.v", r#""near""#]);
    put(&d, &cwd, &["t:template.g", r#""{{ 'tpl' }}""#]);
    put(&d, &cwd, &["t.g", r#""data""#]);

    let rows = rows_of(&d, &cwd, "t", &[]);
    assert!(
        rows.iter().any(|l| l.starts_with("t v put near ")),
        "{rows:?}"
    );
    assert!(!rows.iter().any(|l| l.contains(" far ")), "{rows:?}");
    assert!(
        rows.iter().any(|l| l.starts_with("t g rendered > tpl ")),
        "{rows:?}"
    );
    assert!(!rows.iter().any(|l| l.contains(" data ")), "{rows:?}");

    let all = rows_of(&d, &cwd, "t", &["-a"]);
    assert!(all.iter().any(|l| l.starts_with("t v put far ")), "{all:?}");
    assert!(
        all.iter().any(|l| l.starts_with("t g put data ")),
        "{all:?}"
    );

    let shadowed = rows_of(&d, &cwd, "t", &["--filter", "shadowed=true"]);
    assert_eq!(shadowed.len(), 2, "{shadowed:?}");
    assert!(
        shadowed
            .iter()
            .all(|l| l.contains(" far ") || l.contains(" data "))
    );

    // Under `-a --tree` a shadowed row's children say what shadows it, and
    // never repeat the row itself.
    let out = status(
        &d,
        &cwd,
        &[
            "-a",
            "--tree",
            "--filter",
            "provider=t",
            "--filter",
            "field=v",
            "--color",
            "never",
            "--ascii",
            "--no-trunc",
        ],
    );
    let lines = squashed(&out);
    let far = lines
        .iter()
        .position(|l| l.starts_with("t v put far "))
        .unwrap_or_else(|| panic!("a far row in:\n{out}"));
    assert!(lines[far + 1].starts_with("`- put near "), "{out}");
}

/// A put at a Source's provider root replaces the whole node, so a read of
/// the Source's other leaves is a miss — and the default status does not list
/// them either.
#[test]
fn a_leaf_a_put_replaced_is_not_listed_by_default() {
    let socket = crate::common::socket::IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    std::fs::create_dir_all(xdg.join("beachcomber")).unwrap();
    std::fs::write(
        xdg.join("beachcomber").join("config.toml"),
        r#"
[daemon.providers.s.sources.main]
data = "{{ 1 }}"
result.a = "{{ 'src-a' }}"
result.b = "{{ 'src-b' }}"
"#,
    )
    .unwrap();
    let d = TestDaemon::spawn_in(socket, home, xdg);
    let cwd = project(&d);
    comb(&d, &cwd)
        .args(["get", "--global", "s"])
        .assert()
        .success();
    put(&d, &cwd, &["--global", "s", r#"{"a":"put-a"}"#]);
    comb(&d, &cwd)
        .args(["get", "-f", "json", "s.b"])
        .assert()
        .success()
        .stdout("null\n");

    let rows = rows_of(&d, &cwd, "s", &[]);
    assert!(
        rows.iter().any(|l| l.starts_with("s a put put-a ")),
        "{rows:?}"
    );
    assert!(!rows.iter().any(|l| l.contains("src-b")), "{rows:?}");
    let all = rows_of(&d, &cwd, "s", &["-a"]);
    assert!(all.iter().any(|l| l.contains("src-b")), "{all:?}");
}
