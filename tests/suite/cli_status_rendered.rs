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
    assert_row(&out, "scanline widgets.foo template > hi ");
    assert_row(&out, "scanline theme template > dark ");
    // A depth-2 leaf renders too, not just the top-level ones.
    assert_row(&out, "scanline widgets.clock.format template > %H:%M ");
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
    assert_eq!(absent[0]["kind"]["kind"], "template");

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
    assert_row(&out, "t nothing template > null ");
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
    assert_row(&out, "scanline widgets.foo template \u{25b8} hi ");
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
fn rendered_rows_carry_kind_and_source_template_in_json() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    let rows = json_rows(&d, &cwd, &[]);
    let rendered: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|r| r["provider"] == "scanline" && r["field"] == "widgets.foo")
        .collect();
    assert_eq!(rendered.len(), 1, "one rendered widgets.foo row: {rows:#?}");
    assert_eq!(rendered[0]["source"], "template");
    assert_eq!(rendered[0]["kind"]["kind"], "template");
    assert_eq!(rendered[0]["value"], "hi");
}

#[test]
fn the_lifecycle_filter_selects_only_the_rendered_rows() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    seed_scanline(&d, &cwd);

    let rows = json_rows(&d, &cwd, &["--filter", "lifecycle=template"]);
    assert!(!rows.is_empty(), "the fixture has rendered rows");
    for row in &rows {
        assert_eq!(row["source"], "template", "in {row}");
        assert_eq!(row["kind"]["kind"], "template", "in {row}");
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
        rows.iter().filter(|r| r["source"] == "template").collect();
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
        rows.iter().filter(|r| r["source"] == "template").collect();
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
            .any(|r| r["provider"] == "offstage" && r["source"] == "template"),
        "a template at another path must not be rendered here: {rows:#?}"
    );
}

#[test]
fn a_cached_field_with_a_template_over_it_shows_both_rows() {
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
                .any(|r| r["field"] == "branch" && r["source"] != "template")
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
    let cached: Vec<&serde_json::Value> =
        rows.iter().filter(|r| r["source"] != "template").collect();
    let rendered: Vec<&serde_json::Value> =
        rows.iter().filter(|r| r["source"] == "template").collect();
    assert_eq!(cached.len(), 1, "the cached row stays: {rows:#?}");
    assert_eq!(rendered.len(), 1, "the rendered row joins it: {rows:#?}");
    assert_eq!(rendered[0]["value"], "overridden");
    assert_eq!(rendered[0]["kind"]["kind"], "template");
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
        rows.iter().filter(|r| r["source"] == "template").collect();
    assert_eq!(
        rendered.len(),
        1,
        "a global template must render at a cwd below $HOME: {rows:#?}"
    );
    assert_eq!(rendered[0]["value"], "hi");
}
