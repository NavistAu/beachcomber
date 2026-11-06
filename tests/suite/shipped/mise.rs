//! `defaults/mise.toml`: the tools mise has in effect. The daemon runs a
//! stand-in `mise`, first on its `PATH`, that prints recorded
//! `mise ls --current --json` output: the `.mise-ls.json` of the directory it
//! runs in, else `{}`.

use super::{Scenario, scenario};
use serde_json::{Value, json};
use std::path::Path;
use tempfile::TempDir;

/// One tool's entry as `mise ls --current --json` records it.
fn entry(version: &str, config: &Path) -> Value {
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
fn daemon_path(bin: Option<&Path>) {
    let path = match bin {
        Some(bin) => format!("{}:/usr/bin:/bin", bin.display()),
        None => "/usr/bin:/bin".to_string(),
    };
    // SAFETY: nextest runs each test in its own process, and the daemon that
    // reads this `PATH` has not started.
    unsafe { std::env::set_var("PATH", path) };
}

/// A stand-in `mise` in a directory of its own, `body` its script.
fn stub_mise(body: &str) -> TempDir {
    use std::os::unix::fs::PermissionsExt;
    let bin = TempDir::new().unwrap();
    let mise = bin.path().join("mise");
    std::fs::write(&mise, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&mise, std::fs::Permissions::from_mode(0o755)).unwrap();
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

/// The project's instance answers what mise resolves there, a tool named
/// with a colon and dots included; the instance at `~` answers what it
/// resolves at `~`.
#[test]
fn one_source_answers_project_then_global() {
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
        Some(json!({"node": "20.1.0", "go": "1.22.0", "go:golang.org/x/tools/gopls": "0.16.0"}))
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

/// The instances cascade: a tool the project's instance lacks comes from
/// the instance at `~`.
#[test]
fn a_tool_the_project_lacks_comes_from_home() {
    let (s, _bin) = recorded(
        |s| json!({"node": entry("20.1.0", &s.project.join("mise.toml"))}),
        json!({"go": [{"version": "1.22.0"}]}),
    );
    assert_eq!(s.get("mise.node", &s.project), Some(json!("20.1.0")));
    assert_eq!(s.get("mise.go", &s.project), Some(json!("1.22.0")));
}

/// The deliberate difference: an instance records every tool mise resolves
/// at its directory, a global one included, where the Rust Source kept only
/// what that directory's own files set. A read answers the same; the
/// project's record, which `comb status` lists, holds `go` too.
#[test]
fn the_project_instance_records_every_tool_mise_resolves_there() {
    let (s, _bin) = recorded(
        |s| {
            json!({
                "node": entry("20.1.0", &s.project.join("mise.toml")),
                "go": entry("1.22.0", &global_config(s)),
            })
        },
        json!({"go": [{"version": "1.22.0"}]}),
    );
    assert_eq!(s.get("mise.go", &s.project), Some(json!("1.22.0")));
    let out = s
        .daemon
        .command(&s.project)
        .args(["status", "-f", "json", "--filter", "provider=mise"])
        .output()
        .expect("run comb status");
    let rows: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let project = s.project.display().to_string();
    let mut at_project: Vec<&str> = rows
        .iter()
        .filter(|r| r["path"] == json!(project))
        .filter_map(|r| r["field"].as_str())
        .collect();
    at_project.sort_unstable();
    assert_eq!(at_project, ["go", "node"], "{rows:#?}");
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

/// The shipped Source: one, `tools`, rooted, answering for the provider node
/// whose tool names it cannot list; watching mise's two file names under its
/// root and mise's global config directory, as the Rust Source did.
#[test]
fn mise_provider_is_one_source_with_a_tool_sentinel() {
    let registry = crate::common::registry::with_shipped_defaults();
    let sources = registry.provider_sources("mise").expect("mise");
    assert_eq!(sources.len(), 1);
    let tools = &sources[0];
    assert_eq!(tools.name, "tools");
    assert!(tools.fields.iter().all(|f| f.name.starts_with('<')));
    assert!(registry.source("mise", "tools").unwrap().has_root());
    match &tools.invalidation {
        beachcomber::provider::InvalidationStrategy::Watch {
            patterns,
            abs_paths,
        } => {
            assert_eq!(
                patterns,
                &vec!["mise.toml".to_string(), ".mise.toml".into()]
            );
            assert_eq!(abs_paths.len(), 1);
            assert!(abs_paths[0].ends_with("/mise"), "{abs_paths:?}");
        }
        other => panic!("expected Watch, got {other:?}"),
    }
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
