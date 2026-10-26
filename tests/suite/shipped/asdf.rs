//! `defaults/asdf.toml`: the `tools` Source, one instance per `.tool-versions`
//! and the one at `~`, cascading: each tool from the nearest instance that
//! sets it.

use super::{age, scenario, schedule, served};
use beachcomber::provider::{FailbackConfig, InvalidationStrategy};
use serde_json::json;
use std::path::Path;

/// A `.tool-versions` in `dir` holding `text`.
fn tool_versions(dir: &Path, text: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(".tool-versions"), text).unwrap();
}

/// Each tool is a field of its own, its version a string: no `tools` object.
#[test]
fn asdf_tools_emitted_as_flat_fields_not_object() {
    let s = served(&[], "");
    tool_versions(&s.project, "node 20.11.0\npython 3.12.1\n");
    assert_eq!(
        s.get("asdf", Some(&s.project)),
        Some(json!({"node": "20.11.0", "python": "3.12.1"}))
    );
    assert_eq!(
        s.get("asdf.node", Some(&s.project)),
        Some(json!("20.11.0")),
        "a tool's key routes to the Source"
    );
}

/// Blank lines, comments and a tool with no version are passed over, and of
/// several versions the first is the one in effect.
#[test]
fn asdf_comments_blank_lines_and_fallback_versions() {
    let s = served(&[], "");
    tool_versions(
        &s.project,
        "# pinned for CI\n\n  nodejs   20.0.0 18.19.0\n#ruby 3.0.0\nlonely\n\truby 3.2.0 # trailing\n",
    );
    assert_eq!(
        s.get("asdf", Some(&s.project)),
        Some(json!({"nodejs": "20.0.0", "ruby": "3.2.0"}))
    );
}

/// A directory below a `.tool-versions` reads that project's instance.
#[test]
fn asdf_root_returns_project_root_from_subdir() {
    let s = served(&[], "");
    tool_versions(&s.project, "nodejs 20.0.0\n");
    let sub = s.project.join("pkg");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(s.get("asdf.nodejs", Some(&sub)), Some(json!("20.0.0")));
    s.ran_at("asdf", Some(&s.project), "tools");
}

/// A directory in no project reads the instance at `~`, the global file's.
#[test]
fn asdf_global_fallback_from_home_tool_versions() {
    let s = served(&[], "");
    tool_versions(&s.home, "ruby 3.2.0\n");
    assert_eq!(s.get("asdf.ruby", Some(&s.project)), Some(json!("3.2.0")));
    s.ran_at("asdf", None, "tools");
}

/// With no `~/.tool-versions`, the global file is asdf's XDG one.
#[test]
fn asdf_global_fallback_from_the_xdg_tool_versions() {
    let s = served(&[], "");
    let xdg = s.home.join(".config/asdf");
    std::fs::create_dir_all(&xdg).unwrap();
    std::fs::write(xdg.join("tool-versions"), "golang 1.22.1\n").unwrap();
    assert_eq!(
        s.get("asdf", Some(&s.project)),
        Some(json!({"golang": "1.22.1"}))
    );
    tool_versions(&s.home, "golang 1.21.0\n");
    assert_eq!(
        s.get("asdf.golang", None),
        Some(json!("1.21.0")),
        "~/.tool-versions comes first"
    );
}

/// No `.tool-versions` anywhere: nothing, not an error.
#[test]
fn asdf_root_none_outside_project() {
    let s = served(&[], "");
    assert_eq!(s.get("asdf", Some(&s.project)), None);
    assert_eq!(s.get("asdf", None), None);
}

/// The project's tools win over the global ones, and a tool it does not set
/// is the global one: `asdf.nodejs` from the project, `asdf.golang` from `~`.
#[test]
fn asdf_detects_tool_versions() {
    let s = served(&[], "");
    tool_versions(&s.home, "nodejs 18.0.0\ngolang 1.22.1\n");
    tool_versions(&s.project, "nodejs 20.0.0\nruby 3.2.0\n");
    assert_eq!(
        s.get("asdf", Some(&s.project)),
        Some(json!({"nodejs": "20.0.0", "ruby": "3.2.0", "golang": "1.22.1"}))
    );
    assert_eq!(
        s.get("asdf.golang", Some(&s.project)),
        Some(json!("1.22.1"))
    );
    assert_eq!(s.get("asdf.nodejs", Some(&s.home)), Some(json!("18.0.0")));
}

/// With nothing watching and nothing polling, a rewritten `.tool-versions` is
/// seen by the next read, and a read with nothing changed runs nothing.
#[test]
fn asdf_a_rewritten_tool_versions_is_seen_by_the_next_read() {
    let s = served(&[], "");
    tool_versions(&s.project, "nodejs 20.0.0\n");
    let file = s.project.join(".tool-versions");
    age(&file);
    assert_eq!(
        s.get("asdf.nodejs", Some(&s.project)),
        Some(json!("20.0.0"))
    );
    let ran = s.ran_at("asdf", Some(&s.project), "tools");
    for _ in 0..5 {
        s.get("asdf.nodejs", Some(&s.project));
    }
    assert_eq!(s.ran_at("asdf", Some(&s.project), "tools"), ran);
    std::fs::write(&file, "nodejs 22.1.0\n").unwrap();
    assert_eq!(
        s.get("asdf.nodejs", Some(&s.project)),
        Some(json!("22.1.0"))
    );
}

/// `python.version` falls through to `comb.asdf.python`: the template reads
/// the Source's record like any other.
#[test]
fn asdf_python_version_falls_through_to_asdf() {
    let s = scenario();
    tool_versions(&s.project, "python 3.11.9\n");
    assert_eq!(s.get("python.version", &s.project), Some(json!("3.11.9")));
    assert_eq!(s.get("asdf.python", &s.project), Some(json!("3.11.9")));
}

/// The Rust provider's schedule, kept: watch `.tool-versions`, retry a
/// failure every minute three times, and cascade.
#[test]
fn asdf_keeps_its_schedule() {
    let (meta, rooted) = schedule("asdf", "tools");
    assert!(rooted);
    assert_eq!(
        meta.invalidation,
        InvalidationStrategy::Watch {
            patterns: vec![".tool-versions".into()],
            abs_paths: vec![],
        }
    );
    assert_eq!(
        meta.failback,
        FailbackConfig {
            reattempts: 3,
            interval_secs: 60
        }
    );
    assert!(!meta.nearest, "the instances cascade");
    assert_eq!(meta.decay, None, "the provider's decay prototype");
}
