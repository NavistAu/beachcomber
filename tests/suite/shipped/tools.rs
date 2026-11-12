//! `defaults/tools.toml`: a project's languages, by its markers, and their
//! tools' versions. The daemon runs a stand-in `mise` printing the
//! `.mise-ls.json` of the directory it runs in (else `{}`), and a stand-in
//! `npm` printing a version and a `ruby` printing none.

use super::mise::{daemon_path, entry, stub_mise};
use super::{Scenario, scenario};
use serde_json::{Value, json};
use tempfile::TempDir;

/// A scenario whose daemon runs the stand-ins.
fn with_stubs() -> (Scenario, TempDir) {
    let bin = stub_mise("/bin/cat ./.mise-ls.json 2>/dev/null || echo '{}'");
    crate::common::daemon::tool(bin.path(), "npm", "#!/bin/sh\necho 10.2.0\n");
    // A ruby on PATH that gives no version: the system's is not asked.
    crate::common::daemon::tool(bin.path(), "ruby", "#!/bin/sh\necho 'no version here'\n");
    daemon_path(Some(bin.path()));
    (scenario(), bin)
}

fn write(dir: &std::path::Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), body).unwrap();
}

/// A Rust project shows rust from mise, global or not, and cargo at rust's
/// version; mise's other tools, a project override included, are left out.
/// cargo, in step with rust, is listed but not shown.
#[test]
fn a_rust_project_shows_rust_and_cargo_only() {
    let (s, _bin) = with_stubs();
    write(&s.project, "Cargo.toml", "");
    let ls = json!({
        "rust": entry("1.95.0", &s.project.join("mise.toml")),
        "cargo:cargo-nextest": entry("0.9.1", &s.project.join("mise.toml")),
        "go": entry("1.22.0", &s.project.join("mise.toml")),
    });
    write(&s.project, ".mise-ls.json", &ls.to_string());

    let src = s.project.join("src");
    std::fs::create_dir_all(&src).unwrap();
    assert_eq!(s.get("tools.langs", &s.project), Some(json!(["rust"])));
    assert_eq!(
        s.get("tools.versions", &src),
        Some(json!({"rust": "1.95.0", "cargo": "1.95.0"}))
    );
    let cargo = &s.get("tools.list", &s.project).unwrap()[1];
    assert_eq!(
        (&cargo["name"], &cargo["parent"], &cargo["lang"]),
        (&json!("cargo"), &json!("rust"), &json!("rust"))
    );
    assert_eq!(
        s.get("tools.style.label.full", &s.project),
        Some(json!("rust 1.95.0"))
    );
    assert_eq!(
        s.get("tools.style.label.minor", &s.project),
        Some(json!("rust 1.95"))
    );
    assert_eq!(
        s.get("tools.style.label.major", &s.project),
        Some(json!("rust 1"))
    );
    assert_eq!(
        s.get("tools.style.emoji.minor", &s.project),
        Some(json!("🦀 1.95"))
    );
    assert_eq!(
        s.get("tools.prompt", &s.project),
        Some(json!("🦀 1.95")),
        "the prompt is the emoji style, major.minor"
    );
    let glyphs = s.get("tools.style.glyph.full", &s.project);
    assert!(
        glyphs
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|g| g.ends_with(" 1.95.0") && !g.contains("rust")),
        "{glyphs:?}"
    );
}

/// The prompt is a redirect: a put of another style's name moves it.
#[test]
fn a_put_points_the_prompt_at_another_style() {
    let (s, _bin) = with_stubs();
    write(&s.project, "Cargo.toml", "");
    let ls = json!({"rust": entry("1.95.0", &s.project.join("mise.toml"))});
    write(&s.project, ".mise-ls.json", &ls.to_string());
    let out = s
        .daemon
        .command(&s.project)
        .args([
            "put",
            "--global",
            "tools:template.prompt",
            r#""{{ tools.style.label.full }}""#,
        ])
        .output()
        .expect("run comb put");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        s.get("tools.prompt", &s.project),
        Some(json!("rust 1.95.0"))
    );
}

/// The nearest marker is the project: a node package inside a Rust project
/// shows node, from mise, and npm, from PATH.
#[test]
fn a_nested_package_is_its_own_project() {
    let (s, _bin) = with_stubs();
    write(&s.project, "Cargo.toml", "");
    let site = s.project.join("website");
    write(&site, "package.json", "{}");
    let ls = json!({"node": entry("24.1.0", &s.daemon.xdg.join("mise/config.toml"))});
    write(&site, ".mise-ls.json", &ls.to_string());

    assert_eq!(s.get("tools.langs", &site), Some(json!(["node"])));
    let list = s.get("tools.list", &site).expect("a list");
    let shown: Vec<_> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap(),
                t["version"].as_str().unwrap(),
                t["minor"].as_str().unwrap(),
                t["via"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("node", "24.1.0", "24.1", "mise"),
            ("npm", "10.2.0", "10.2", "path")
        ]
    );
    // npm, at a version of its own, shows beside node; node has no emoji,
    // so the emoji form gives its glyph, coloured (unless NO_COLOR).
    assert_eq!(
        s.get("tools.style.label.major", &site),
        Some(json!("node 24 npm 10"))
    );
    let emoji = s.get("tools.style.emoji.full", &site);
    assert!(
        emoji
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|e| e.contains(" 24.1.0") && !e.contains("node")),
        "{emoji:?}"
    );
}

/// A tool with no version to be found is left out, and with no tools the
/// display fields are absent; a directory with no marker
/// above it has no tools.
#[test]
fn what_cannot_be_found_is_absent() {
    let (s, _bin) = with_stubs();
    write(&s.project, "Gemfile", "");
    assert_eq!(s.get("tools.langs", &s.project), Some(json!(["ruby"])));
    assert_eq!(s.get("tools.list", &s.project), Some(json!([])));
    assert_eq!(s.get("tools.style.label.full", &s.project), None);
    let elsewhere = s.daemon.home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    assert_eq!(s.get("tools.langs", &elsewhere), None);
}
