//! `defaults/mise.toml`: the tools mise has in effect. The daemon runs a
//! stand-in `mise`, first on its `PATH`, that prints recorded
//! `mise ls --current --json` output: the `.mise-ls.json` of the directory it
//! runs in, else `{}`.

use super::{Scenario, scenario};
use serde_json::{Value, json};
use std::path::Path;
use tempfile::TempDir;

/// One tool's entry as `mise ls --current --json` records it.
pub(super) fn entry(version: &str, config: &Path) -> Value {
    json!([{
        "version": version,
        "requested_version": version,
        "install_path": format!("/mise/installs/tool/{version}"),
        "source": {"type": "mise.toml", "path": config},
        "installed": true,
        "active": true,
    }])
}

/// `PATH` for the daemon this process starts: `bin`, if any, then the system
/// directories, and never the developer's own `mise`.
pub(super) fn daemon_path(bin: Option<&Path>) {
    let path = match bin {
        Some(bin) => format!(
            "{}:{}",
            bin.display(),
            crate::common::daemon::system_bin().display()
        ),
        None => crate::common::daemon::system_bin().display().to_string(),
    };
    // SAFETY: nextest runs each test in its own process, and the daemon that
    // reads this `PATH` has not started.
    unsafe { std::env::set_var("PATH", path) };
}

/// A stand-in `mise` in a directory of its own, `body` its script.
pub(super) fn stub_mise(body: &str) -> TempDir {
    let bin = TempDir::new().unwrap();
    crate::common::daemon::tool(bin.path(), "mise", &format!("#!/bin/sh\n{body}\n"));
    bin
}

/// A scenario whose daemon runs the recording stand-in: `project` holds a
/// `mise.toml` and mise's answer there, `project_ls`; `~`'s answer is
/// `home_ls`.
fn recorded(project_ls: impl Fn(&Scenario) -> Value, home_ls: Value) -> (Scenario, TempDir) {
    let bin = stub_mise("/bin/cat ./.mise-ls.json 2>/dev/null || echo '{}'");
    daemon_path(Some(bin.path()));
    let s = scenario();
    std::fs::write(s.project.join("mise.toml"), "[tools]\n").unwrap();
    std::fs::write(s.project.join(".mise-ls.json"), project_ls(&s).to_string()).unwrap();
    std::fs::write(s.daemon.home.join(".mise-ls.json"), home_ls.to_string()).unwrap();
    (s, bin)
}

fn global_config(s: &Scenario) -> std::path::PathBuf {
    s.daemon.xdg.join("mise/config.toml")
}

/// The project's instance holds the tools its own mise file sets, a tool
/// named with a colon and dots included, and leaves out what comes from a
/// global file; the instance at `~` holds every tool mise resolves there.
#[test]
fn a_project_holds_its_own_overrides_and_home_the_globals() {
    let (s, _bin) = recorded(
        |s| {
            let mut ls = serde_json::Map::new();
            ls.insert("node".into(), entry("20.1.0", &s.project.join("mise.toml")));
            ls.insert("go".into(), entry("1.22.0", &global_config(s)));
            ls.insert(
                "go:golang.org/x/tools/gopls".into(),
                entry("0.16.0", &s.project.join("mise.toml")),
            );
            Value::Object(ls)
        },
        json!({"go": [{"version": "1.22.0"}], "ruby": []}),
    );
    let sub = s.project.join("src");
    std::fs::create_dir_all(&sub).unwrap();

    assert_eq!(
        s.get("mise", &sub),
        Some(json!({"node": "20.1.0", "go:golang.org/x/tools/gopls": "0.16.0"}))
    );
    assert_eq!(
        s.get("mise['go:golang.org/x/tools/gopls']", &sub),
        Some(json!("0.16.0"))
    );
    assert_eq!(
        s.get("mise", &s.daemon.home),
        Some(json!({"go": "1.22.0"})),
        "a tool with no entry is absent"
    );
}

/// The project answers alone: a tool only the global config sets is absent
/// there, not filled in from `~`; a directory in no project reads `~`'s.
#[test]
fn the_project_answers_alone_and_elsewhere_reads_home() {
    let (s, _bin) = recorded(
        |s| json!({"node": entry("20.1.0", &s.project.join("mise.toml"))}),
        json!({"go": [{"version": "1.22.0"}]}),
    );
    assert_eq!(s.get("mise.node", &s.project), Some(json!("20.1.0")));
    assert_eq!(s.get("mise.go", &s.project), None);
    let elsewhere = s.daemon.home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    assert_eq!(s.get("mise", &elsewhere), Some(json!({"go": "1.22.0"})));
}

/// No `mise` to run, or one that fails: the Source fails, and a read is a
/// miss, not an error.
#[test]
fn a_tools_run_that_fails_is_a_miss_not_an_error() {
    let bin = stub_mise("echo 'mise: not trusted' >&2; exit 1");
    daemon_path(Some(bin.path()));
    let s = scenario();
    std::fs::write(s.project.join("mise.toml"), "").unwrap();
    for key in ["mise", "mise.node"] {
        let out = s
            .daemon
            .command(&s.project)
            .args(["get", "-f", "json", key])
            .output()
            .expect("run comb get");
        assert_eq!(out.status.code(), Some(0), "{key}: {out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "null", "{key}");
    }
}

#[test]
fn mise_absent_from_path_is_a_miss() {
    daemon_path(None);
    let s = scenario();
    std::fs::write(s.project.join("mise.toml"), "").unwrap();
    assert_eq!(s.get("mise", &s.project), None);
}

/// The shipped Source: one, `tools`, rooted and nearest, answering for the
/// provider node whose tool names it cannot list; watching mise's file names
/// under its root and mise's global config directory.
#[test]
fn mise_provider_is_one_source_with_a_tool_sentinel() {
    let registry = crate::common::registry::with_shipped_defaults();
    let sources = registry.provider_sources("mise").expect("mise");
    assert_eq!(sources.len(), 1);
    let tools = &sources[0];
    assert_eq!(tools.name, "tools");
    assert!(tools.fields.iter().all(|f| f.name.starts_with('<')));
    assert!(registry.source("mise", "tools").unwrap().has_root());
    let schedule = &tools.schedule;
    assert!(schedule.poll.is_none(), "expected Watch, got {schedule:?}");
    assert_eq!(
        schedule.watch,
        vec![
            "mise.toml".to_string(),
            ".mise.toml".into(),
            ".config/mise.toml".into()
        ]
    );
    assert!(schedule.nearest);
    assert_eq!(schedule.watch_abs.len(), 1);
    assert!(
        schedule.watch_abs[0].ends_with("/mise"),
        "{:?}",
        schedule.watch_abs
    );
}

/// The root is the nearest directory holding either file, from a
/// subdirectory or the directory itself; a directory with neither above it
/// has none of its own.
#[test]
fn the_tools_source_roots_at_the_nearest_mise_toml_directory() {
    let registry = crate::common::registry::with_shipped_defaults();
    let tools = registry.source("mise", "tools").unwrap();
    let root_of = |dir: &Path| tools.root(dir.to_str().unwrap()).unwrap();

    let d = TempDir::new().unwrap();
    let outer = d.path().canonicalize().unwrap();
    let inner = outer.join("inner");
    let deep = inner.join("src/deep");
    std::fs::create_dir_all(&deep).unwrap();
    assert_ne!(root_of(&deep).as_deref(), Some(deep.to_str().unwrap()));

    std::fs::write(outer.join(".mise.toml"), "").unwrap();
    assert_eq!(root_of(&deep), Some(outer.display().to_string()));
    std::fs::write(inner.join("mise.toml"), "").unwrap();
    assert_eq!(root_of(&deep), Some(inner.display().to_string()));
    assert_eq!(root_of(&inner), Some(inner.display().to_string()));

    std::fs::remove_file(inner.join("mise.toml")).unwrap();
    std::fs::write(inner.join(".mise.toml"), "").unwrap();
    std::fs::write(outer.join("mise.toml"), "").unwrap();
    assert_eq!(
        root_of(&deep),
        Some(inner.display().to_string()),
        "the nearer of the two names wins, whichever it is"
    );
}
