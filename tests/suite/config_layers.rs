//! `[daemon.providers.<p>.sources.<s>]`: a Source is a function. These tests
//! spawn real `comb` daemons over a config directory and read through
//! `comb get`, so they cover the layers (defaults, `config.toml`, `conf.d`),
//! the moments each line runs at, and whose environment `data` sees.
//!
//! A `sources` edit restarts the daemon (reload arrives with a later task), so
//! a layer scenario is a daemon started over the files as they are.

use crate::common::daemon::TestDaemon;
use crate::common::socket::IsolatedSocket;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// A `$HOME` and a config directory that outlive any one daemon over them.
struct Setup {
    _base: TempDir,
    home: PathBuf,
    xdg: PathBuf,
}

impl Setup {
    /// Write `files` (paths relative to `<xdg>/beachcomber`).
    fn new(files: &[(&str, &str)]) -> Self {
        let base = tempfile::tempdir().expect("tempdir");
        let root = base.path().canonicalize().unwrap();
        let setup = Setup {
            home: root.join("home"),
            xdg: root.join("xdg"),
            _base: base,
        };
        std::fs::create_dir_all(&setup.home).unwrap();
        for (rel, body) in files {
            setup.write(rel, body);
        }
        setup
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.xdg.join("beachcomber").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn remove(&self, rel: &str) {
        std::fs::remove_file(self.xdg.join("beachcomber").join(rel)).unwrap();
    }

    fn daemon(&self) -> TestDaemon {
        TestDaemon::spawn_in(IsolatedSocket::new(), self.home.clone(), self.xdg.clone())
    }
}

/// `comb get -f json <args>` from `cwd`: the parsed value, or `None` on a miss.
fn get_at(d: &TestDaemon, cwd: &Path, args: &[&str]) -> Option<Value> {
    let out = d
        .comb(cwd)
        .args(["get", "-f", "json"])
        .args(args)
        .output()
        .expect("run comb get");
    out.status.success().then(|| {
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("get {args:?}: not JSON ({e}): {:?}", out.stdout))
    })
}

fn get(d: &TestDaemon, key: &str) -> Option<Value> {
    get_at(d, &d.home, &[key])
}

const BASE: &str = r#"
[daemon.providers.demo.sources.s]
data = "{{ exec('echo base') | trim }}"
result.a = "{{ data }}-a"
result.b = "{{ data }}-b"
"#;

#[test]
fn one_result_line_in_conf_d_replaces_one_expression() {
    let setup = Setup::new(&[
        ("config.toml", BASE),
        (
            "conf.d/10-mine.toml",
            "[daemon.providers.demo.sources.s]\nresult.b = \"{{ data }}-mine\"\n",
        ),
    ]);
    let d = setup.daemon();
    assert_eq!(
        get(&d, "demo.a"),
        Some(json!("base-a")),
        "a sibling is kept"
    );
    assert_eq!(
        get(&d, "demo.b"),
        Some(json!("base-mine")),
        "one line replaced"
    );
}

#[test]
fn a_return_line_replaces_the_schedule_whole() {
    let setup = Setup::new(&[
        (
            "config.toml",
            r#"
[daemon.providers.demo.sources.plain]
data = "{{ exec('echo p') | trim }}"
result.a = "{{ data }}"

[daemon.providers.demo.sources.own]
data = "{{ exec('echo o') | trim }}"
result.b = "{{ data }}"
"#,
        ),
        (
            "conf.d/10-mine.toml",
            "[daemon.providers.demo.sources.own]\nreturn = \"{{ result | watch('data.txt') }}\"\n",
        ),
    ]);
    let d = setup.daemon();
    assert_eq!(get(&d, "demo.a"), Some(json!("p")));
    assert_eq!(get(&d, "demo.b"), Some(json!("o")));

    let plain = get(&d, "demo:cache.a").expect("demo:cache.a");
    assert_eq!(
        plain["poll"],
        json!({"interval": "60s", "count": 12}),
        "no return of its own: the abstract Source's schedule: {plain}"
    );
    assert!(plain.get("watch").is_none(), "{plain}");

    let own = get(&d, "demo:cache.b").expect("demo:cache.b");
    assert_eq!(own["watch"], json!({"0": "data.txt"}), "{own}");
    assert!(
        own.get("poll").is_none(),
        "the Source's own return replaces the abstract one whole, poll included: {own}"
    );
}

#[test]
fn a_new_source_table_adds_a_source() {
    let setup = Setup::new(&[
        ("config.toml", BASE),
        (
            "conf.d/10-more.toml",
            r#"
[daemon.providers.demo.sources.t]
data = "{{ exec('echo more') | trim }}"
result.c = "{{ data }}"
"#,
        ),
    ]);
    let d = setup.daemon();
    assert_eq!(get(&d, "demo.a"), Some(json!("base-a")));
    assert_eq!(get(&d, "demo.c"), Some(json!("more")));
    let whole = get(&d, "demo").expect("demo");
    assert_eq!(whole["c"], json!("more"), "{whole}");
    assert_eq!(whole["a"], json!("base-a"), "{whole}");
}

#[test]
fn removing_a_file_removes_what_it_added() {
    let setup = Setup::new(&[
        ("config.toml", BASE),
        (
            "conf.d/10-more.toml",
            "[daemon.providers.demo.sources.t]\ndata = \"{{ exec('echo more') | trim }}\"\n\
             result.c = \"{{ data }}\"\n\
             [daemon.providers.demo.sources.s]\nresult.b = \"{{ data }}-more\"\n",
        ),
    ]);
    {
        let d = setup.daemon();
        assert_eq!(get(&d, "demo.c"), Some(json!("more")));
        assert_eq!(get(&d, "demo.b"), Some(json!("base-more")));
    }
    setup.remove("conf.d/10-more.toml");
    let d = setup.daemon();
    assert_eq!(get(&d, "demo.c"), None, "the Source the file added is gone");
    assert_eq!(
        get(&d, "demo.b"),
        Some(json!("base-b")),
        "the line the file replaced is the layer below's again"
    );
}

#[test]
fn a_put_over_a_file_value_wins() {
    let setup = Setup::new(&[("config.toml", BASE)]);
    let d = setup.daemon();
    assert_eq!(get(&d, "demo.a"), Some(json!("base-a")));
    d.comb(&d.home)
        .args(["put", "demo.a", "\"over\""])
        .assert()
        .success();
    assert_eq!(
        get(&d, "demo.a"),
        Some(json!("over")),
        "put beats the Source"
    );
    d.comb(&d.home)
        .args(["put", "demo.a", "null"])
        .assert()
        .success();
    assert_eq!(
        get(&d, "demo.a"),
        Some(json!("base-a")),
        "removing the put shows the Source again"
    );
}

#[test]
fn a_data_line_reads_a_put_over_another_providers_source() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.one.sources.s]
data = "{{ exec('echo src') | trim }}"
result.x = "{{ data }}"

[daemon.providers.two.sources.s]
data = "{{ comb.one.x }}"
result.y = "{{ data }}!"
"#,
    )]);
    let d = setup.daemon();
    assert_eq!(get(&d, "one.x"), Some(json!("src")));
    d.comb(&d.home)
        .args(["put", "one.x", "\"over\""])
        .assert()
        .success();
    assert_eq!(
        get(&d, "two.y"),
        Some(json!("over!")),
        "comb.one.x is the put over the Source, through the daemon's own read"
    );
}

#[test]
fn data_runs_at_refresh_not_at_load() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.demo.sources.s]
data = "{{ exec('echo run >> ' ~ env.HOME ~ '/count; echo hi') | trim }}"
result.v = "{{ data }}"
"#,
    )]);
    let counter = setup.home.join("count");
    let d = setup.daemon();
    // The daemon is up (it answers) and nothing has asked for demo yet.
    assert_eq!(get(&d, "nothing.here"), None);
    assert!(!counter.exists(), "load did not run data");

    assert_eq!(get(&d, "demo.v"), Some(json!("hi")));
    assert!(counter.exists(), "the first read ran data");
}

#[test]
fn the_daemons_env_not_the_callers() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.demo.sources.s]
data = "{{ {'h': env.HOME, 'm': env.MARK} }}"
result.h = "{{ data.h }}"
result.m = "{{ data.m }}"
"#,
    )]);
    let d = setup.daemon();
    let get_as_caller = |key: &str| {
        let out = d
            .comb(&d.home)
            .env("HOME", "/caller/home")
            .env("MARK", "caller")
            .args(["get", "-f", "json", key])
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| serde_json::from_slice::<Value>(&out.stdout).unwrap())
    };
    assert_eq!(
        get_as_caller("demo.h"),
        Some(json!(setup.home.to_str().unwrap())),
        "env.HOME is the daemon's"
    );
    assert_eq!(
        get_as_caller("demo.m"),
        None,
        "the caller's MARK never reaches the Source: absent, not 'caller'"
    );
}

#[test]
fn a_root_line_picks_the_instance_and_none_is_the_home_instance() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.demo.sources.s]
root = "{{ lup('marker') | dirname }}"
data = "{{ exec('pwd') | trim }}"
result.cwd = "{{ data }}"
"#,
    )]);
    let d = setup.daemon();
    let a = setup.home.join("a");
    let b = setup.home.join("b");
    for dir in [&a, &b] {
        std::fs::create_dir_all(dir.join("deep/er")).unwrap();
        std::fs::write(dir.join("marker"), "").unwrap();
    }
    let outside_tmp = tempfile::tempdir().unwrap();
    let outside = outside_tmp.path().canonicalize().unwrap();

    // Below a marker: the instance answers for the marked directory, and data
    // ran there.
    for dir in [&a, &b] {
        let deep = dir.join("deep/er");
        assert_eq!(
            get_at(&d, &deep, &["demo.cwd"]),
            Some(json!(dir.to_str().unwrap())),
            "{dir:?}"
        );
        let meta = get_at(&d, dir, &["demo:cache.cwd"]).expect("the record at the root");
        assert_eq!(meta["source"], json!("source(s)"), "{meta}");
        assert_eq!(
            get_at(&d, &deep, &["demo:cache.cwd"]).expect("same instance")["source"],
            json!("source(s)")
        );
    }

    // Outside both, the root renders none: the one instance at ~.
    assert_eq!(
        get_at(&d, &outside, &["demo.cwd"]),
        Some(json!(setup.home.to_str().unwrap())),
        "data ran with $HOME as its working directory"
    );
    let at_home = get_at(&d, &d.home, &["--global", "demo:cache.cwd"]).expect("record at ~");
    assert_eq!(at_home["source"], json!("source(s)"), "{at_home}");
    assert!(
        get_at(&d, &outside, &["demo:cache.cwd"]).is_some(),
        "outside reads the global entry"
    );
}

#[test]
fn a_source_with_no_root_line_is_the_home_instance() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.demo.sources.s]
data = "{{ exec('pwd') | trim }}"
result.cwd = "{{ data }}"
"#,
    )]);
    let d = setup.daemon();
    let deep = setup.home.join("proj/sub");
    std::fs::create_dir_all(&deep).unwrap();
    assert_eq!(
        get_at(&d, &deep, &["demo.cwd"]),
        Some(json!(setup.home.to_str().unwrap()))
    );
    let at_home = get_at(&d, &d.home, &["--global", "demo:cache.cwd"]).expect("record at ~");
    assert_eq!(at_home["source"], json!("source(s)"), "{at_home}");
}

#[test]
fn a_url_source_serves_the_fetched_field() {
    let mut server = mockito::Server::new();
    let _mock = server
        .mock("GET", "/summary.json")
        .with_status(200)
        .with_body(r#"{"status":{"indicator":"minor"}}"#)
        .create();
    let setup = Setup::new(&[(
        "config.toml",
        &format!(
            r#"
[daemon.providers.claude.sources.summary]
data = "{{{{ url('{}/summary.json') | fromjson }}}}"
result.indicator = "{{{{ data.status.indicator }}}}"
return = "{{{{ result | poll('60s') }}}}"
"#,
            server.url()
        ),
    )]);
    let d = setup.daemon();
    assert_eq!(get(&d, "claude.indicator"), Some(json!("minor")));
}
