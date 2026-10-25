//! `comb check config --origins`: how the composed configuration came
//! together — every key, the file that set it, and the earlier files it
//! overrode. Client-side, so every test here runs with no daemon behind the
//! socket.

use crate::common::daemon::isolate;
use std::path::Path;
use tempfile::TempDir;

/// A config home whose `beachcomber/` holds `files` — `(relative path,
/// contents)`, e.g. `("defaults/demo.toml", "…")`.
fn config_home(files: &[(&str, &str)]) -> TempDir {
    let tmp = TempDir::new().unwrap();
    for (rel, body) in files {
        let path = tmp.path().join("beachcomber").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    tmp
}

/// `comb check config --origins <args…>` over `home`, with no daemon.
fn origins(home: &Path, args: &[&str]) -> std::process::Output {
    isolate(
        &mut std::process::Command::new(env!("CARGO_BIN_EXE_comb")),
        home,
        home,
        &home.join("no-daemon.sock"),
    )
    .args(["check", "--connect-timeout", "0", "config", "--origins"])
    .args(args)
    .output()
    .expect("run comb check config --origins")
}

/// Text output, colour off and ASCII glyphs, every run of whitespace squashed
/// to one space so the columns can be asserted on without their padding.
fn text(home: &Path, args: &[&str]) -> Vec<String> {
    // A KEY comes first: it is `--origins`' own value.
    let mut all = args.to_vec();
    all.extend_from_slice(&["--color", "never", "--ascii"]);
    let out = origins(home, &all);
    assert!(
        out.status.success(),
        "exit {:?}, stderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

fn json(home: &Path, args: &[&str]) -> Vec<serde_json::Value> {
    let mut all = args.to_vec();
    all.extend_from_slice(&["-f", "json"]);
    let out = origins(home, &all);
    assert!(out.status.success(), "exit {:?}", out.status.code());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("one JSON object per line"))
        .collect()
}

/// The same leaf in `defaults/`, `config.toml` and `conf.d/`.
fn three_files() -> TempDir {
    config_home(&[
        (
            "defaults/demo.toml",
            "[client.providers.demo]\ntemplate.x = \"{{ 'd' }}\"\ntemplate.only = \"{{ 1 }}\"\n",
        ),
        (
            "config.toml",
            "[client.providers.demo]\ntemplate.x = \"{{ 'c' }}\"\n",
        ),
        (
            "conf.d/10-a.toml",
            "[client.providers.demo]\ntemplate.x = \"{{ 'a' }}\"\n",
        ),
    ])
}

#[test]
fn a_leaf_three_files_set_shows_the_winner_then_what_it_overrode() {
    let home = three_files();
    let lines = text(home.path(), &[]);

    let at = lines
        .iter()
        .position(|l| l.starts_with("client.providers.demo.template.x "))
        .unwrap_or_else(|| panic!("a demo.x line in:\n{}", lines.join("\n")));
    assert_eq!(
        lines[at],
        "client.providers.demo.template.x ~/beachcomber/conf.d/10-a.toml {{ 'a' }}"
    );
    assert_eq!(
        lines[at + 1],
        "|- shadowed ~/beachcomber/config.toml {{ 'c' }}"
    );
    assert_eq!(
        lines[at + 2],
        "`- shadowed ~/beachcomber/defaults/demo.toml {{ 'd' }}"
    );
    // A leaf one file set has nothing beneath it.
    let only = lines
        .iter()
        .position(|l| l.starts_with("client.providers.demo.template.only "))
        .expect("the only leaf");
    assert_eq!(
        lines[only],
        "client.providers.demo.template.only ~/beachcomber/defaults/demo.toml {{ 1 }}"
    );
    assert!(
        !lines.get(only + 1).is_some_and(|l| l.contains("shadowed")),
        "{}",
        lines.join("\n")
    );
}

#[test]
fn a_key_limits_the_output_to_its_subtree() {
    let home = three_files();
    let lines = text(home.path(), &["client.providers.demo.template.x"]);
    let keys: Vec<&String> = lines
        .iter()
        .filter(|l| !l.starts_with("KEY ") && !l.starts_with("|-") && !l.starts_with("`-"))
        .collect();
    assert_eq!(
        keys,
        vec!["client.providers.demo.template.x ~/beachcomber/conf.d/10-a.toml {{ 'a' }}"]
    );

    let lines = text(home.path(), &["client.providers.demo"]);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("client.providers.demo.template.only "))
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("daemon.")),
        "nothing outside the subtree: {}",
        lines.join("\n")
    );
}

#[test]
fn json_is_one_object_per_leaf_with_what_it_shadowed() {
    let home = three_files();
    let rows = json(home.path(), &["client.providers.demo.template.x"]);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    let row = &rows[0];
    assert_eq!(row["key"], "client.providers.demo.template.x");
    assert_eq!(row["value"], "{{ 'a' }}");
    let base = home.path().join("beachcomber");
    assert_eq!(
        row["file"],
        base.join("conf.d/10-a.toml").display().to_string()
    );
    let shadowed = row["shadowed"].as_array().expect("shadowed");
    assert_eq!(
        shadowed,
        &vec![
            serde_json::json!({
                "file": base.join("config.toml").display().to_string(),
                "value": "{{ 'c' }}",
            }),
            serde_json::json!({
                "file": base.join("defaults/demo.toml").display().to_string(),
                "value": "{{ 'd' }}",
            }),
        ]
    );
}

#[test]
fn a_value_no_file_sets_is_comb_s_default() {
    let home = config_home(&[]);
    let lines = text(home.path(), &["daemon.log_level"]);
    assert!(
        lines.iter().any(|l| l == "daemon.log_level default info"),
        "{}",
        lines.join("\n")
    );
    let rows = json(home.path(), &["daemon.timeout"]);
    assert_eq!(
        rows,
        vec![serde_json::json!({
            "key": "daemon.timeout",
            "value": "10s",
            "file": null,
            "shadowed": [],
        })]
    );

    // Set in a file, the file wins and the default is not listed beneath it.
    let home = config_home(&[("config.toml", "[daemon]\nlog_level = \"debug\"\n")]);
    let lines = text(home.path(), &["daemon.log_level"]);
    assert_eq!(
        lines
            .iter()
            .filter(|l| !l.starts_with("KEY "))
            .collect::<Vec<_>>(),
        vec!["daemon.log_level ~/beachcomber/config.toml debug"]
    );
}

#[test]
fn a_name_holding_a_dot_prints_as_a_quoted_segment() {
    let home = config_home(&[(
        "config.toml",
        "[client.providers.demo]\ntemplate.\"a.b\" = \"{{ 1 }}\"\n",
    )]);
    let lines = text(home.path(), &["client.providers.demo"]);
    assert!(
        lines
            .iter()
            .any(|l| l == "client.providers.demo.template['a.b'] ~/beachcomber/config.toml {{ 1 }}"),
        "{}",
        lines.join("\n")
    );
    // And the quoted form is a key the filter takes.
    let rows = json(home.path(), &["client.providers.demo.template['a.b']"]);
    assert_eq!(rows.len(), 1, "{rows:#?}");
}

#[test]
fn a_broken_file_is_reported_and_fails() {
    let home = config_home(&[
        ("config.toml", "[daemon]\nlog_level = \"info\"\n"),
        ("conf.d/10-bad.toml", "[daemon\n"),
    ]);
    let out = origins(home.path(), &["--color", "never"]);
    assert_eq!(out.status.code(), Some(2), "a config error exits 2");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[FAIL] local") && stdout.contains("10-bad.toml"),
        "the error, as check config reports it:\n{stdout}"
    );
    // The files that did parse are still shown.
    assert!(stdout.contains("daemon.log_level"), "{stdout}");
}

#[test]
fn origin_options_need_origins() {
    let home = config_home(&[]);
    let out = isolate(
        &mut std::process::Command::new(env!("CARGO_BIN_EXE_comb")),
        home.path(),
        home.path(),
        &home.path().join("no-daemon.sock"),
    )
    .args(["check", "--connect-timeout", "0", "config", "-f", "json"])
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(2), "a clap usage error");
}
