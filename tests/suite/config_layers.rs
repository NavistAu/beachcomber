//! `[daemon.providers.<p>.sources.<s>]`: a Source is a function. These tests
//! run a daemon in the test process over a config directory (one test spawns a
//! real `comb daemon`) and read through `comb get`, so they cover the layers (defaults, `config.toml`, `conf.d`),
//! the moments each line runs at, and whose environment `data` sees.
//!
//! A scenario here is a daemon started over the files as they are. An edit
//! under `providers` reloads a running daemon in place; that is covered in
//! `config_self_watch.rs`.

use crate::common::daemon::TestDaemon;
use crate::common::poll::poll_until;
use crate::common::socket::IsolatedSocket;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;
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

    /// A daemon over these files, running inside the test process.
    fn daemon(&self) -> TestDaemon {
        TestDaemon::serve_in_process(IsolatedSocket::new(), self.home.clone(), self.xdg.clone())
    }

    /// A real `comb daemon` process, for a test about the process itself.
    fn spawned_daemon(&self) -> TestDaemon {
        TestDaemon::spawn_in(IsolatedSocket::new(), self.home.clone(), self.xdg.clone())
    }
}

/// `comb get -f json <args>` from `cwd`: the parsed value, or `None` on a miss
/// (`null`) or an error.
fn get_at(d: &TestDaemon, cwd: &Path, args: &[&str]) -> Option<Value> {
    let out = d
        .comb(cwd)
        .args(["get", "-f", "json"])
        .args(args)
        .output()
        .expect("run comb get");
    out.status
        .success()
        .then(|| {
            serde_json::from_slice::<Value>(&out.stdout)
                .unwrap_or_else(|e| panic!("get {args:?}: not JSON ({e}): {:?}", out.stdout))
        })
        .filter(|v| !v.is_null())
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
            "[daemon.providers.demo.sources.own]\nreturn = \"{{ result | watch('/etc/hosts') }}\"\n",
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
    assert_eq!(own["watch"], json!(["/etc/hosts"]), "{own}");
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

/// `comb.<p>` in a `data` line is a read like any other: a provider nothing
/// has read yet runs, once, for the line that reads it.
#[test]
fn a_data_line_reading_a_cold_provider_runs_it() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.up.sources.s]
data = "{{ exec('echo upstream') | trim }}"
result.v = "{{ data }}"

[daemon.providers.down.sources.s]
data = "{{ comb.up.v }}"
result.v = "{{ data }}!"

[daemon.providers.dyn.sources.s]
data = "{{ comb['up2'].v }}"
result.v = "{{ data }}?"

[daemon.providers.up2.sources.s]
data = "{{ exec('echo second') | trim }}"
result.v = "{{ data }}"
"#,
    )]);
    let d = setup.daemon();
    assert_eq!(get(&d, "down.v"), Some(json!("upstream!")));
    assert_eq!(
        get(&d, "dyn.v"),
        Some(json!("second?")),
        "and comb[...] too"
    );
}

/// A Source never waits on itself: one that reads its own provider, or reads
/// round a ring of `comb.` reads back to itself, finds its own last stored
/// value, which before its first run is nothing.
#[test]
fn a_data_line_reading_its_own_provider_finds_its_last_value() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.me.sources.s]
data = "{{ (comb.me.n or 0) + 1 }}"
result.n = "{{ data }}"

[daemon.providers.ring_a.sources.s]
data = "{{ comb.ring_b.v or 'none' }}"
result.v = "{{ 'a<' ~ data }}"

[daemon.providers.ring_b.sources.s]
data = "{{ comb.ring_a.v or 'none' }}"
result.v = "{{ 'b<' ~ data }}"
"#,
    )]);
    let d = setup.daemon();
    assert_eq!(get(&d, "me.n"), Some(json!(1)));
    assert_eq!(get(&d, "ring_a.v"), Some(json!("a<b<none")));
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
    let d = setup.spawned_daemon();
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
            .filter(|v| !v.is_null())
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

/// What `comb check` reads from the daemon for `subject`.
fn introspect(d: &TestDaemon, subject: &str) -> Value {
    beachcomber::client::Client::new(d.socket.path.clone())
        .introspect(subject, None)
        .unwrap_or_else(|e| panic!("introspect {subject}: {e}"))
        .data
        .unwrap_or_else(|| panic!("introspect {subject}: no payload"))
}

/// The paths the daemon watches for Sources.
fn watched(d: &TestDaemon) -> Vec<String> {
    introspect(d, "watches")["paths"]
        .as_array()
        .expect("paths")
        .iter()
        .map(|p| p.as_str().expect("a path").to_string())
        .collect()
}

/// The `(source, root)` of every instance the daemon is keeping warm for
/// `provider`, sorted; a root of `None` is the instance at `~`.
fn warm_instances(d: &TestDaemon, provider: &str) -> Vec<(String, Option<String>)> {
    let mut rows: Vec<(String, Option<String>)> = introspect(d, "demand")["demand"]
        .as_array()
        .map(|rows| rows.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|row| row["provider"] == provider)
        .map(|row| {
            (
                row["source"].as_str().expect("source").to_string(),
                row["path"].as_str().map(str::to_string),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// Lines in `file`, or 0 when it has not been written.
fn lines_in(file: &Path) -> usize {
    std::fs::read_to_string(file).map_or(0, |text| text.lines().count())
}

#[test]
fn root_runs_per_directory_on_first_request_and_is_cached() {
    let setup = Setup::new(&[(
        "config.toml",
        // Each render appends a line to a counter file, then names the
        // directory the instance answers for.
        r#"
[daemon.providers.demo.sources.s]
root = "{{ exec('echo run >> ' ~ env.HOME ~ '/roots; echo ' ~ env.HOME ~ '/proj') | trim }}"
data = "{{ exec('pwd') | trim }}"
result.cwd = "{{ data }}"
"#,
    )]);
    let proj = setup.home.join("proj");
    let deep = proj.join("a/b");
    let other = proj.join("c");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let roots = setup.home.join("roots");
    let d = setup.daemon();
    assert_eq!(lines_in(&roots), 0, "load does not render a root");

    let want = Some(json!(proj.to_str().unwrap()));
    assert_eq!(get_at(&d, &deep, &["demo.cwd"]), want);
    assert_eq!(
        lines_in(&roots),
        1,
        "the first request for a directory renders it"
    );

    assert_eq!(get_at(&d, &deep, &["demo.cwd"]), want);
    assert_eq!(
        get_at(&d, &deep, &["demo:cache.cwd"]).map(|m| m["source"].clone()),
        Some(json!("source(s)"))
    );
    assert_eq!(
        lines_in(&roots),
        1,
        "later reads from that directory use the root already found"
    );

    assert_eq!(get_at(&d, &other, &["demo.cwd"]), want);
    assert_eq!(
        lines_in(&roots),
        2,
        "a different directory renders its own, once"
    );
}

#[test]
fn return_decorators_register_at_instance_creation_not_at_load() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.demo.sources.s]
root = "{{ lup('marker') | dirname }}"
data = "{{ exec('pwd') | trim }}"
result.cwd = "{{ data }}"
return = "{{ result | watch('marker') | poll('1h') }}"
"#,
    )]);
    let proj = setup.home.join("proj");
    let deep = proj.join("a/b");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(proj.join("marker"), "").unwrap();
    let d = setup.daemon();

    assert_eq!(watched(&d), Vec::<String>::new(), "no watch after load");
    assert_eq!(warm_instances(&d, "demo"), vec![], "no instance after load");

    assert_eq!(
        get_at(&d, &deep, &["demo.cwd"]),
        Some(json!(proj.to_str().unwrap()))
    );
    assert_eq!(
        warm_instances(&d, "demo"),
        vec![("s".to_string(), Some(proj.to_str().unwrap().to_string()))],
        "the first request created the instance at the root"
    );
    assert_eq!(
        watched(&d),
        vec![proj.to_str().unwrap().to_string()],
        "and registered its watch there, at the root and not at the subdirectory"
    );
}

#[test]
fn two_config_sources_of_one_provider_root_independently() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.demo.sources.a]
root = "{{ lup('ma') | dirname }}"
data = "{{ exec('pwd') | trim }}"
result.cwd_a = "{{ data }}"

[daemon.providers.demo.sources.b]
root = "{{ lup('mb') | dirname }}"
data = "{{ exec('pwd') | trim }}"
result.cwd_b = "{{ data }}"
"#,
    )]);
    let outer = setup.home.join("p");
    let inner = outer.join("q");
    let deep = inner.join("deep");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(outer.join("ma"), "").unwrap();
    std::fs::write(inner.join("mb"), "").unwrap();
    let d = setup.daemon();

    assert_eq!(
        get_at(&d, &deep, &["demo.cwd_a"]),
        Some(json!(outer.to_str().unwrap())),
        "a roots at the nearest `ma`"
    );
    assert_eq!(
        get_at(&d, &deep, &["demo.cwd_b"]),
        Some(json!(inner.to_str().unwrap())),
        "b roots at the nearest `mb`, a directory nearer than a's"
    );
    assert_eq!(
        warm_instances(&d, "demo"),
        vec![
            ("a".to_string(), Some(outer.to_str().unwrap().to_string())),
            ("b".to_string(), Some(inner.to_str().unwrap().to_string())),
        ],
        "each Source has its own instance, at its own root"
    );
}

#[test]
fn a_config_source_walks_to_the_home_instance() {
    let setup = Setup::new(&[(
        "config.toml",
        // The whole node is whatever `vals.json` in the working directory says.
        r#"
[daemon.providers.demo.sources.s]
root = "{{ lup('vals.json') | dirname }}"
data = "{{ exec('cat vals.json 2>/dev/null || echo {}') | fromjson }}"
result = "{{ data }}"
"#,
    )]);
    let proj = setup.home.join("proj");
    let deep = proj.join("sub");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(proj.join("vals.json"), r#"{"a": "proj-a"}"#).unwrap();
    std::fs::write(
        setup.home.join("vals.json"),
        r#"{"a": "home-a", "b": "home-b"}"#,
    )
    .unwrap();
    let d = setup.daemon();

    assert_eq!(
        get_at(&d, &deep, &["demo.a"]),
        Some(json!("proj-a")),
        "the project's value is the nearer"
    );
    assert_eq!(
        get_at(&d, &deep, &["demo.b"]),
        Some(json!("home-b")),
        "a field missing at the project root is read from the instance at ~"
    );
    assert_eq!(
        get_at(&d, &deep, &["demo"]),
        Some(json!({"a": "proj-a", "b": "home-b"})),
        "the whole node merges the instances up the chain"
    );
    assert_eq!(get_at(&d, &deep, &["demo.zz"]), None, "a key nobody has");
}

/// A config Source over `vals.json`: the whole node is the nearest marker's
/// file. `return` is the line under test; `nearest` appends `| nearest`.
fn vals_config(provider: &str, nearest: bool) -> String {
    let decorator = if nearest { " | nearest" } else { "" };
    format!(
        r#"
[daemon.providers.{provider}.sources.s]
root = "{{{{ lup('vals.json') | dirname }}}}"
data = "{{{{ exec('cat vals.json') | fromjson }}}}"
result = "{{{{ data }}}}"
return = "{{{{ result | watch('vals.json'){decorator} }}}}"
"#
    )
}

/// `outer/vals.json` has `a` and `b`; `outer/inner/vals.json` only `a`.
/// Returns the directory to read from, below `inner`.
fn nested_markers(setup: &Setup) -> PathBuf {
    let outer = setup.home.join("outer");
    let deep = outer.join("inner/sub");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(
        outer.join("vals.json"),
        r#"{"a": "outer-a", "b": "outer-b"}"#,
    )
    .unwrap();
    std::fs::write(outer.join("inner/vals.json"), r#"{"a": "inner-a"}"#).unwrap();
    deep
}

#[test]
fn a_nearest_config_source_is_answered_by_its_nearest_root_alone() {
    let setup = Setup::new(&[("config.toml", &vals_config("demo", true))]);
    let deep = nested_markers(&setup);
    let d = setup.daemon();

    assert_eq!(get_at(&d, &deep, &["demo.a"]), Some(json!("inner-a")));
    assert_eq!(
        get_at(&d, &deep, &["demo.b"]),
        None,
        "b is missing at the nearest root, and the outer root does not fill in"
    );
    assert_eq!(
        get_at(&d, &deep, &["demo"]),
        Some(json!({"a": "inner-a"})),
        "the namespace read is the nearest instance only"
    );
}

#[test]
fn the_same_config_source_without_nearest_cascades() {
    let setup = Setup::new(&[("config.toml", &vals_config("demo", false))]);
    let deep = nested_markers(&setup);
    let d = setup.daemon();

    assert_eq!(get_at(&d, &deep, &["demo.a"]), Some(json!("inner-a")));
    assert_eq!(get_at(&d, &deep, &["demo.b"]), Some(json!("outer-b")));
    assert_eq!(
        get_at(&d, &deep, &["demo"]),
        Some(json!({"a": "inner-a", "b": "outer-b"}))
    );
}

#[test]
fn a_nearest_config_source_whose_root_renders_none_has_no_instance() {
    let setup = Setup::new(&[(
        "config.toml",
        &format!(
            "{}{}",
            vals_config("near", true),
            r#"
[daemon.providers.casc.sources.s]
root = "{{ lup('vals.json') | dirname }}"
data = "{{ 1 }}"
result.z = "ran"
"#
        ),
    )]);
    let d = setup.daemon();
    let bare = setup.home.join("bare/dir");
    std::fs::create_dir_all(&bare).unwrap();

    assert_eq!(
        get_at(&d, &bare, &["near"]),
        None,
        "no marker: no instance, so no value"
    );
    assert_eq!(
        get_at(&d, &bare, &["casc.z"]),
        Some(json!("ran")),
        "a Source that cascades is served by its instance at ~ instead"
    );
    assert_eq!(
        warm_instances(&d, "near"),
        vec![],
        "nothing was created or kept warm for the nearest Source"
    );
    assert_eq!(
        warm_instances(&d, "casc"),
        vec![("s".to_string(), None)],
        "the cascading Source has its instance at ~"
    );
}

#[test]
fn a_root_line_that_fails_to_render_fails_the_read_with_its_error() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.demo.sources.s]
root = "{{ exec('false') }}"
data = "{{ exec('pwd') | trim }}"
result.cwd = "{{ data }}"
"#,
    )]);
    let dir = setup.home.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let d = setup.daemon();

    let out = d
        .comb(&dir)
        .args(["get", "-f", "json", "demo.cwd"])
        .output()
        .expect("run comb get");
    assert!(!out.status.success(), "the read fails: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("demo.s: root: "),
        "the root's own error: {stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains(setup.home.to_str().unwrap()),
        "and nothing was answered from the instance at ~"
    );
    assert_eq!(
        warm_instances(&d, "demo"),
        vec![],
        "no instance was created for a root that could not be found"
    );
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

/// `<dir>/lib<name>.<ext>`: the fixture cdylib, linked under the name
/// `plugin('<name>', ...)` searches for.
fn link_plugin(dir: &Path, name: &str) {
    use crate::common::fixture::{LIB_EXT, ensure_fixture_built};
    std::fs::create_dir_all(dir).unwrap();
    std::os::unix::fs::symlink(
        ensure_fixture_built(),
        dir.join(format!("lib{name}.{LIB_EXT}")),
    )
    .unwrap();
}

const PLUGIN_SOURCE: &str = r#"
[daemon.providers.lib.sources.s]
data = "{{ plugin('demo', 'bc_echo_args', 'x', 2) }}"
result.first = "{{ data[0] }}"
result.second = "{{ data[1] }}"
result.sum = "{{ plugin('demo', 'bc_sum', 1, 2) }}"
"#;

#[test]
fn a_plugin_source_serves_the_library_result() {
    let setup = Setup::new(&[(
        "config.toml",
        &format!("[daemon.plugin]\npath = [\"~/libs\"]\n{PLUGIN_SOURCE}"),
    )]);
    link_plugin(&setup.home.join("libs"), "demo");
    let d = setup.daemon();
    assert_eq!(get(&d, "lib.first"), Some(json!("x")));
    assert_eq!(get(&d, "lib.second"), Some(json!(2)));
    assert_eq!(get(&d, "lib.sum"), Some(json!(3)));
}

#[test]
fn plugin_looks_in_the_config_directorys_plugins_by_default() {
    let setup = Setup::new(&[("config.toml", PLUGIN_SOURCE)]);
    link_plugin(&setup.xdg.join("beachcomber").join("plugins"), "demo");
    let d = setup.daemon();
    assert_eq!(get(&d, "lib.first"), Some(json!("x")));
}

/// A Source whose whole node is the JSON in `vals.json` at its root, rooted at
/// the nearest `.root` marker.
const OBJ_SOURCE: &str = r#"
[daemon.providers.obj.sources.s]
root = "{{ lup('.root') | dirname }}"
data = "{{ exec('cat vals.json') | fromjson }}"
result = "{{ data }}"
"#;

/// One scenario per process: the daemon reads `$HOME` once, so a second daemon
/// over a different `$HOME` in one test process would not see it.
fn object_field_read_agrees_with_the_namespace_read(proj_vals: &str, home_vals: &str) {
    let setup = Setup::new(&[("config.toml", OBJ_SOURCE)]);
    let proj = setup.home.join("ws/p");
    let sub = proj.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(proj.join(".root"), "").unwrap();
    std::fs::write(proj.join("vals.json"), proj_vals).unwrap();
    std::fs::write(setup.home.join("vals.json"), home_vals).unwrap();
    let d = setup.daemon();

    // The field read goes first, so it is the cold path that is tested.
    let field = get_at(&d, &sub, &["obj.o"]);
    let namespace = get_at(&d, &sub, &["obj"]).expect("obj");
    assert_eq!(
        field.as_ref(),
        Some(&namespace["o"]),
        "get obj.o and get obj agree ({proj_vals} over {home_vals})"
    );
    assert_eq!(
        get_at(&d, &sub, &["obj.o.b"]),
        Some(json!("home-b")),
        "the home instance contributes a leaf the project lacks"
    );
}

#[test]
fn a_field_read_of_an_object_merges_up_the_chain_like_the_namespace_read() {
    object_field_read_agrees_with_the_namespace_read(
        r#"{"o":{"a":"proj-a"}}"#,
        r#"{"o":{"a":"home-a","b":"home-b"}}"#,
    );
}

#[test]
fn a_field_read_of_an_object_with_a_null_leaf_merges_like_the_namespace_read() {
    object_field_read_agrees_with_the_namespace_read(
        r#"{"o":{"a":null}}"#,
        r#"{"o":{"a":"home-a","b":"home-b"}}"#,
    );
}

#[test]
fn a_nearer_root_beats_the_home_instance_of_a_rootless_sibling_source() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.p.sources.a]
root = "{{ lup('.root') | dirname }}"
data = "{{ exec('cat a.json') | fromjson }}"
result = "{{ data }}"

[daemon.providers.p.sources.b]
data = "{{ exec('cat b.json') | fromjson }}"
result = "{{ data }}"
"#,
    )]);
    let outer = setup.home.join("ws/outer");
    let src = outer.join("inner/src");
    std::fs::create_dir_all(&src).unwrap();
    let inner = outer.join("inner");
    std::fs::write(outer.join(".root"), "").unwrap();
    std::fs::write(inner.join(".root"), "").unwrap();
    std::fs::write(outer.join("a.json"), r#"{"k":"outer-a"}"#).unwrap();
    std::fs::write(inner.join("a.json"), r#"{"other":"inner-a"}"#).unwrap();
    std::fs::write(setup.home.join("b.json"), r#"{"k":"home-b"}"#).unwrap();
    let d = setup.daemon();

    assert_eq!(
        get_at(&d, &src, &["p.k"]),
        Some(json!("outer-a")),
        "the instance at ~ is the farthest, whatever the other Source's chain"
    );
    assert_eq!(
        get_at(&d, &src, &["p"]),
        Some(json!({"k": "outer-a", "other": "inner-a"}))
    );
}

#[test]
fn refresh_re_runs_every_instance_a_force_get_re_runs() {
    let setup = Setup::new(&[]);
    let count = setup.home.join("cnt");
    setup.write(
        "config.toml",
        &format!(
            r#"
[daemon.providers.cnt.sources.s]
root = "{{{{ lup('.root') | dirname }}}}"
data = "{{{{ exec('pwd >> {}; cat vals.json') | fromjson }}}}"
result = "{{{{ data }}}}"
"#,
            count.display()
        ),
    );
    let proj = setup.home.join("ws/p");
    let sub = proj.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(proj.join(".root"), "").unwrap();
    std::fs::write(proj.join("vals.json"), r#"{"y":"proj"}"#).unwrap();
    std::fs::write(setup.home.join("vals.json"), r#"{"x":"home"}"#).unwrap();
    let d = setup.daemon();

    assert_eq!(
        get_at(&d, &sub, &["cnt"]),
        Some(json!({"y": "proj", "x": "home"}))
    );
    let after_read = lines_in(&count);
    let out = d
        .comb(&sub)
        .args(["refresh", "cnt"])
        .output()
        .expect("run comb refresh");
    assert!(out.status.success(), "{out:?}");
    // The refresh is the scheduler's, so it lands when it lands.
    poll_until(
        "the executions after refresh",
        "one at the project and one at ~",
        Duration::from_secs(5),
        Duration::from_millis(20),
        || lines_in(&count) - after_read,
        |n| *n >= 2,
    );
    let ran: Vec<String> = std::fs::read_to_string(&count)
        .unwrap()
        .lines()
        .skip(after_read)
        .map(str::to_string)
        .collect();
    assert!(
        ran.contains(&proj.to_str().unwrap().to_string())
            && ran.contains(&setup.home.to_str().unwrap().to_string()),
        "refresh re-ran the project and the global instance: {ran:?}"
    );
}

#[test]
fn a_failing_source_in_backoff_is_skipped_not_re_run_on_every_read() {
    let setup = Setup::new(&[]);
    let count = setup.home.join("bad.count");
    setup.write(
        "config.toml",
        &format!(
            r#"
[daemon.providers.bad.sources.s]
root = "{{{{ exec('pwd') | trim }}}}"
data = "{{{{ exec('pwd >> {}; false') }}}}"
result.x = "{{{{ data }}}}"
return = "{{{{ result | failback(1, '1h') }}}}"
"#,
            count.display()
        ),
    );
    let deep = setup.home.join("ws/a/b/c");
    std::fs::create_dir_all(&deep).unwrap();
    let d = setup.daemon();

    assert_eq!(get_at(&d, &deep, &["bad.x"]), None);
    let first = lines_in(&count);
    assert!(first >= 2, "the first read tried each level: {first}");
    assert_eq!(get_at(&d, &deep, &["bad.x"]), None);
    assert_eq!(get_at(&d, &deep, &["bad.x"]), None);
    assert_eq!(
        lines_in(&count),
        first,
        "instances in failback backoff are not run again by later reads"
    );
}

#[test]
fn the_home_instance_of_mise_watches_the_config_file_in_home() {
    let setup = Setup::new(&[]);
    std::fs::write(setup.home.join("mise.toml"), "").unwrap();
    let d = setup.daemon();

    // Whether or not `mise` is installed here, the read creates the instance.
    let _ = get_at(&d, &d.home, &["mise"]);
    let home = setup.home.to_str().unwrap();
    let watched = watched(&d);
    assert!(
        watched.contains(&format!("{home}/mise.toml")),
        "the file mise reads from $HOME is watched: {watched:?}"
    );
    assert!(
        !watched.contains(&home.to_string()),
        "and $HOME is not watched recursively: {watched:?}"
    );
}

// ── Provider lines: template, result, return ─────────────────────────────────

/// Two Sources of one provider, each with a field of its own.
const TWO_SOURCES: &str = r#"
[daemon.providers.demo.sources.one]
data = "{{ exec('echo 1') | trim }}"
result.a = "{{ data }}-a"
result.b = "{{ data }}-b"

[daemon.providers.demo.sources.two]
data = "{{ exec('echo 2') | trim }}"
result.c = "{{ data }}-c"
"#;

#[test]
fn default_merge_equals_the_old_merge() {
    let setup = Setup::new(&[("config.toml", TWO_SOURCES)]);
    let d = setup.daemon();
    assert_eq!(
        get(&d, "demo"),
        Some(json!({"a": "1-a", "b": "1-b", "c": "2-c"})),
        "the abstract result merges every Source of the provider"
    );
    assert_eq!(get(&d, "demo.c"), Some(json!("2-c")));
}

#[test]
fn explicit_merge_last_wins() {
    // Both Sources declare `x`: the default merge refuses that, an explicit
    // `merge(a, b)` has chosen an order.
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.demo]
result = "{{ merge(sources.first(), sources.second()) }}"

[daemon.providers.demo.sources.first]
data = "{{ exec('echo f') | trim }}"
result.x = "{{ data }}"
result.only_first = "{{ data }}"

[daemon.providers.demo.sources.second]
data = "{{ exec('echo s') | trim }}"
result.x = "{{ data }}"
"#,
    )]);
    let d = setup.daemon();
    assert_eq!(get(&d, "demo.x"), Some(json!("s")), "the last one wins");
    assert_eq!(get(&d, "demo.only_first"), Some(json!("f")));
}

const TEMPLATED: &str = r#"
[daemon.providers.demo.sources.s]
data = "{{ exec('echo d') | trim }}"
result.a = "{{ data }}"

[daemon.providers.other.sources.s]
data = "{{ exec('echo o') | trim }}"
result.a = "{{ data }}"

[client.providers.demo]
template.t = "{{ 1 + 1 }}"

[client.providers.other]
template.t = "{{ 1 + 1 }}"
"#;

#[test]
fn a_provider_return_without_facet_carries_no_template() {
    let setup = Setup::new(&[
        ("config.toml", TEMPLATED),
        (
            "conf.d/10-mine.toml",
            "[daemon.providers.demo]\nreturn = \"{{ result }}\"\n",
        ),
    ]);
    let d = setup.daemon();
    assert_eq!(get(&d, "other.t"), Some(json!(2)), "the abstract return");
    assert_eq!(get(&d, "demo.a"), Some(json!("d")));
    assert_eq!(
        get(&d, "demo.t"),
        None,
        "no facet('template', ...) in the return: no template half"
    );
    assert_eq!(
        get(&d, "demo:template.t"),
        Some(json!("{{ 1 + 1 }}")),
        "the facet itself still reads"
    );
}

#[test]
fn underscore_name_at_provider_level() {
    let setup = Setup::new(&[(
        "config.toml",
        "[client.providers.aws]\ntemplate.region = \"{{ 'eu' }}\"\n\
         [client.providers.gcp]\ntemplate.region = \"{{ 'us' }}\"\n",
    )]);
    let d = setup.daemon();
    assert_eq!(get(&d, "aws:template.region"), Some(json!("{{ 'eu' }}")));
    assert_eq!(get(&d, "gcp:template.region"), Some(json!("{{ 'us' }}")));
    assert_eq!(get(&d, "aws.region"), Some(json!("eu")));
}

#[test]
fn a_template_line_override_seeds_from_another_client_provider() {
    let setup = Setup::new(&[
        (
            "config.toml",
            "[client.providers.aws]\ntemplate.region = \"{{ 'eu' }}\"\n",
        ),
        (
            "conf.d/10-mine.toml",
            "[daemon.providers.git]\ntemplate = \"{{ client.providers.aws.template }}\"\n",
        ),
    ]);
    let d = setup.daemon();
    assert_eq!(
        get(&d, "git:template.region"),
        Some(json!("{{ 'eu' }}")),
        "git's template is aws's"
    );
    assert_eq!(get(&d, "aws:template.region"), Some(json!("{{ 'eu' }}")));
}

#[test]
fn super_at_provider_level() {
    let setup = Setup::new(&[
        ("config.toml", TEMPLATED),
        (
            "conf.d/10-mine.toml",
            "[daemon.providers.demo]\nreturn = \"{{ result | super }}\"\n",
        ),
    ]);
    let d = setup.daemon();
    assert_eq!(
        get(&d, "demo.t"),
        Some(json!(2)),
        "| super reaches the abstract return, which attaches the template"
    );
}

#[test]
fn a_provider_with_no_sources_renders_its_result_at_a_read() {
    let setup = Setup::new(&[(
        "config.toml",
        "[daemon.providers.lit]\nresult = \"{{ {'a': 1} }}\"\n\n[daemon.providers.bare]\n",
    )]);
    let d = setup.daemon();
    assert_eq!(get(&d, "lit.a"), Some(json!(1)), "a literal is answered");
    assert_eq!(get(&d, "lit"), Some(json!({"a": 1})));
    assert_eq!(
        get(&d, "bare"),
        None,
        "the default result merges no Sources: {{}} answers nothing"
    );
}

#[test]
fn the_default_provider_lines_reproduce_todays_read() {
    if !crate::common::git::has_git() {
        return;
    }
    let setup = Setup::new(&[("config.toml", BASE)]);
    let d = setup.daemon();
    let repo = crate::common::git::GitRepoFixture::new();
    let nested = repo.path().join("one/two");
    std::fs::create_dir_all(&nested).unwrap();
    let nested = nested.canonicalize().unwrap();

    // A built-in provider, read from a directory below its root.
    assert_eq!(
        get_at(&d, &nested, &["git.branch"]),
        Some(json!("main")),
        "a built-in answers from below its root"
    );

    // A config provider, with a put over one Source field at the nested
    // directory: the put wins there, the other field is the Source's, and the
    // home read is untouched.
    d.comb(&nested)
        .args(["put", "demo.a", "\"over\""])
        .assert()
        .success();
    assert_eq!(
        get_at(&d, &nested, &["demo"]),
        Some(json!({"a": "over", "b": "base-b"})),
    );
    assert_eq!(get_at(&d, &nested, &["demo.b"]), Some(json!("base-b")),);
    assert_eq!(
        get_at(&d, &d.home, &["demo"]),
        Some(json!({"a": "base-a", "b": "base-b"})),
        "away from the put the Source answers alone"
    );
}

/// How many times the Source behind `count` has run: it appends a line per run.
fn runs(count: &Path) -> usize {
    std::fs::read_to_string(count).map_or(0, |text| text.lines().count())
}

#[test]
fn a_field_a_result_line_renames_is_read_by_its_new_name() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.ren]
result = "{{ {'y': sources.one().a} }}"

[daemon.providers.ren.sources.one]
data = "{{ 5 }}"
result.a = "{{ data }}"
"#,
    )]);
    let d = setup.daemon();
    // The field read comes first, on a cold daemon: no Source owns `y`, so the
    // read has to run every Source before the line can say what exists.
    assert_eq!(get(&d, "ren.y"), Some(json!(5)), "a field the line makes");
    assert_eq!(get(&d, "ren"), Some(json!({"y": 5})));
    assert_eq!(get(&d, "ren.a"), None, "and the one it drops is gone");
}

#[test]
fn a_field_a_result_line_drops_does_not_rerun_its_source_on_every_read() {
    let setup = Setup::new(&[]);
    let count = setup.home.join("count");
    setup.write(
        "config.toml",
        &format!(
            r#"
[daemon.providers.drop]
result = "{{{{ {{'kept': 1}} }}}}"

[daemon.providers.drop.sources.one]
data = "{{{{ exec(['sh', '-c', 'echo run >> {} ; echo 5']) | trim }}}}"
result.a = "{{{{ data }}}}"
"#,
            count.display()
        ),
    );
    let d = setup.daemon();
    assert_eq!(get(&d, "drop.kept"), Some(json!(1)));
    let after_first = runs(&count);
    assert!(after_first >= 1, "the Source ran for the first read");
    for _ in 0..3 {
        assert_eq!(get(&d, "drop.a"), None, "the line dropped it");
    }
    assert_eq!(
        runs(&count),
        after_first,
        "a miss the line explains is not a reason to run the Source again"
    );
}

#[test]
fn a_return_line_hands_out_the_data_it_computes() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.p]
return = "{{ result | merge({'x': 1}) | facet('template', template) }}"

[daemon.providers.p.sources.s]
data = "{{ 2 }}"
result.a = "{{ data }}"

[daemon.providers.q]
return = "{{ {'z': 3} }}"

[client.providers.p]
template.t = "{{ 7 }}"
"#,
    )]);
    let d = setup.daemon();
    assert_eq!(get(&d, "p.x"), Some(json!(1)), "a field the return adds");
    assert_eq!(get(&d, "p.a"), Some(json!(2)), "and the data it hands on");
    assert_eq!(get(&d, "p.t"), Some(json!(7)), "the template rides beside");
    assert_eq!(
        get(&d, "q.z"),
        Some(json!(3)),
        "a provider with no Sources answers what its return computes"
    );
}

#[test]
fn a_data_line_sees_what_the_providers_return_gives() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.p]
return = "{{ result | merge({'x': 1}) | facet('template', template) }}"

[daemon.providers.p.sources.s]
data = "{{ 2 }}"
result.a = "{{ data }}"

[daemon.providers.q.sources.s]
data = "{{ comb.p }}"
result.seen = "{{ data.x }}"
"#,
    )]);
    let d = setup.daemon();
    assert_eq!(get(&d, "p"), Some(json!({"a": 2, "x": 1})));
    assert_eq!(
        get(&d, "q.seen"),
        Some(json!(1)),
        "comb.p is the answer to a get of p, not its stored records"
    );
}

#[test]
fn a_return_that_fails_is_a_miss() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.bad]
return = "{{ 1 // 0 }}"

[daemon.providers.bad.sources.s]
data = "{{ 2 }}"
result.a = "{{ data }}"
"#,
    )]);
    let d = setup.daemon();
    assert_eq!(get(&d, "bad.a"), None, "the data is not served around it");
    assert_eq!(get(&d, "bad"), None);
}

#[test]
fn a_provider_result_can_merge_another_providers_source() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.gitstats.sources.repo]
data = "{{ 7 }}"
result.commits = "{{ data }}"

[daemon.providers.report]
return = "{{ result | merge(daemon.providers.gitstats.sources.repo()) | facet('template', template) }}"

[daemon.providers.report.sources.own]
data = "{{ 2 }}"
result.mine = "{{ data }}"

[daemon.providers.layered]
result = "{{ merge(sources) | merge(daemon.providers.gitstats.sources.repo()) }}"

[daemon.providers.layered.sources.own]
data = "{{ 3 }}"
result.mine = "{{ data }}"
"#,
    )]);
    let d = setup.daemon();
    // Warm: the called Source has been read before the lines call it.
    assert_eq!(get(&d, "gitstats.commits"), Some(json!(7)));
    assert_eq!(
        get(&d, "report"),
        Some(json!({"mine": 2, "commits": 7})),
        "a return merges its own Sources with another provider's Source"
    );
    assert_eq!(
        get(&d, "layered"),
        Some(json!({"mine": 3, "commits": 7})),
        "and so does a result"
    );
}

#[test]
fn a_cold_source_call_executes_once_inline() {
    let setup = Setup::new(&[]);
    let from_data = setup.home.join("from_data.count");
    let from_return = setup.home.join("from_return.count");
    let body = |count: &Path| {
        format!(
            "{{{{ exec(['sh', '-c', 'echo run >> {} ; echo 5']) | trim }}}}",
            count.display()
        )
    };
    setup.write(
        "config.toml",
        &format!(
            r#"
[daemon.providers.a.sources.s]
data = "{{{{ daemon.providers.b.sources.t().v ~ '-' ~ daemon.providers.b.sources.t().v }}}}"
result.x = "{{{{ data }}}}"

[daemon.providers.b.sources.t]
data = "{data_body}"
result.v = "{{{{ data }}}}"

[daemon.providers.c]
return = "{{{{ merge(daemon.providers.d.sources.u(), daemon.providers.d.sources.u()) }}}}"

[daemon.providers.c.sources.own]
data = "{{{{ 1 }}}}"
result.mine = "{{{{ data }}}}"

[daemon.providers.d.sources.u]
data = "{return_body}"
result.w = "{{{{ data }}}}"
"#,
            data_body = body(&from_data),
            return_body = body(&from_return),
        ),
    );
    let d = setup.daemon();

    assert_eq!(get(&d, "a.x"), Some(json!("5-5")), "the cold Source ran");
    assert_eq!(runs(&from_data), 1, "once for the read, not per call site");
    assert_eq!(get(&d, "a.x"), Some(json!("5-5")));
    assert_eq!(runs(&from_data), 1, "and its record is stored");

    assert_eq!(get(&d, "c"), Some(json!({"w": "5"})));
    assert_eq!(runs(&from_return), 1, "a provider line's call runs it once");
}

/// A Source that appends a line to `count` each time it runs and answers how
/// many runs there have been.
fn counting_source(count: &Path) -> String {
    format!(
        "data = \"{{{{ exec(['sh', '-c', 'echo run >> {c} ; wc -l < {c}']) | trim }}}}\"\nresult.v = \"{{{{ data }}}}\"\n",
        c = count.display()
    )
}

#[test]
fn a_source_called_from_a_return_is_demanded_with_the_read() {
    let setup = Setup::new(&[]);
    let count = setup.home.join("clock.count");
    setup.write(
        "config.toml",
        &format!(
            r#"
[daemon.providers.clock.sources.t]
{}return = "{{{{ result | poll('1s') }}}}"

[daemon.providers.report]
return = "{{{{ result | merge(daemon.providers.clock.sources.t()) }}}}"

[daemon.providers.report.sources.own]
data = "{{{{ 2 }}}}"
result.mine = "{{{{ data }}}}"
"#,
            counting_source(&count)
        ),
    );
    let d = setup.daemon();
    assert_eq!(get(&d, "report"), Some(json!({"mine": 2, "v": "1"})));
    assert_eq!(
        warm_instances(&d, "clock"),
        vec![("t".to_string(), None)],
        "the Source a line calls is demanded by the read that called it"
    );
    // Its own schedule runs it again; a refresh stands in for a poll.
    let out = d
        .comb(&d.home)
        .args(["refresh", "clock"])
        .output()
        .expect("run comb refresh");
    assert!(out.status.success(), "{out:?}");
    poll_until(
        "the called Source",
        "run again",
        Duration::from_secs(20),
        Duration::from_millis(100),
        || get(&d, "report"),
        |v| *v == Some(json!({"mine": 2, "v": "2"})),
    );
}

#[test]
fn a_source_called_from_a_data_line_is_demanded_with_its_caller() {
    let setup = Setup::new(&[]);
    let count = setup.home.join("clock2.count");
    setup.write(
        "config.toml",
        &format!(
            r#"
[daemon.providers.clock2.sources.t]
{}return = "{{{{ result | poll('1s') }}}}"

[daemon.providers.viadata.sources.s]
data = "{{{{ daemon.providers.clock2.sources.t().v }}}}"
result.x = "{{{{ data }}}}"
"#,
            counting_source(&count)
        ),
    );
    let d = setup.daemon();
    assert_eq!(get(&d, "viadata.x"), Some(json!("1")));
    assert_eq!(
        warm_instances(&d, "clock2"),
        vec![("t".to_string(), None)],
        "the caller's read demanded what its run called"
    );
}

#[test]
fn a_callee_is_warmed_after_the_provider_it_reads() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.cr]
return = "{{ result | merge(daemon.providers.seer.sources.t()) }}"

[daemon.providers.cr.sources.own]
data = "{{ 2 }}"
result.mine = "{{ data }}"

[daemon.providers.seer.sources.t]
data = "{{ comb.cr.mine or 'cold' }}"
result.seen = "{{ data }}"
"#,
    )]);
    let d = setup.daemon();
    assert_eq!(
        get(&d, "cr"),
        Some(json!({"mine": 2, "seen": 2})),
        "on the first read the callee finds the provider already warm"
    );
}

#[test]
fn a_refresh_runs_a_shared_cold_source_once() {
    let setup = Setup::new(&[]);
    let count = setup.home.join("shared.count");
    setup.write(
        "config.toml",
        &format!(
            r#"
[daemon.providers.shared.sources.t]
data = "{{{{ exec(['sh', '-c', 'sleep 0.3; echo run >> {}; echo 1']) | trim }}}}"
result.v = "{{{{ data }}}}"

[daemon.providers.callers.sources.one]
data = "{{{{ daemon.providers.shared.sources.t().v }}}}"
result.x1 = "{{{{ data }}}}"

[daemon.providers.callers.sources.two]
data = "{{{{ daemon.providers.shared.sources.t().v }}}}"
result.x2 = "{{{{ data }}}}"
"#,
            count.display()
        ),
    );
    let d = setup.daemon();
    let out = d
        .comb(&d.home)
        .args(["refresh", "callers"])
        .output()
        .expect("run comb refresh");
    assert!(out.status.success(), "{out:?}");
    poll_until(
        "both callers",
        "both stored",
        Duration::from_secs(20),
        Duration::from_millis(100),
        || get(&d, "callers"),
        |v| *v == Some(json!({"x1": "1", "x2": "1"})),
    );
    assert_eq!(
        runs(&count),
        1,
        "one run for the refresh, not one per caller"
    );
}

/// The frames of a `watch` of a key.
struct Watch {
    lines: std::io::BufReader<std::os::unix::net::UnixStream>,
}

impl Watch {
    fn open(d: &TestDaemon, key: &str) -> Self {
        use std::io::Write;
        let mut stream = std::os::unix::net::UnixStream::connect(&d.socket.path).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        writeln!(stream, "{}", json!({"op": "watch", "key": key})).unwrap();
        Watch {
            lines: std::io::BufReader::new(stream),
        }
    }

    /// The data of the next frame.
    fn next(&mut self) -> Value {
        use std::io::BufRead;
        let mut line = String::new();
        self.lines.read_line(&mut line).expect("a watch frame");
        serde_json::from_str::<Value>(&line).expect("a frame")["data"].clone()
    }
}

#[test]
fn a_watch_wakes_when_a_source_its_lines_call_changes() {
    let setup = Setup::new(&[]);
    let count = setup.home.join("tick.count");
    setup.write(
        "config.toml",
        &format!(
            r#"
[daemon.providers.tick.sources.t]
data = "{{{{ exec(['sh', '-c', 'echo run >> {}; wc -l < {}']) | trim }}}}"
result.n = "{{{{ data }}}}"

[daemon.providers.seen]
return = "{{{{ daemon.providers.tick.sources.t() }}}}"
"#,
            count.display(),
            count.display()
        ),
    );
    let d = setup.daemon();
    assert_eq!(get(&d, "seen"), Some(json!({"n": "1"})));
    let mut watch = Watch::open(&d, "seen");
    assert_eq!(watch.next(), json!({"n": "1"}));
    let out = d
        .comb(&d.home)
        .args(["refresh", "tick"])
        .output()
        .expect("run comb refresh");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        watch.next(),
        json!({"n": "2"}),
        "the called Source ran again, and the watch followed"
    );
}

#[test]
fn a_called_nearest_source_answers_from_its_nearest_root_alone() {
    let setup = Setup::new(&[(
        "config.toml",
        &format!(
            r#"{}
[daemon.providers.rep.sources.c]
root = "{{{{ lup('.here') | dirname }}}}"
data = "{{{{ daemon.providers.near.sources.s() }}}}"
result = "{{{{ data }}}}"

[daemon.providers.nores]
result = "{{{{ daemon.providers.near.sources.s() }}}}"
"#,
            vals_config("near", true)
        ),
    )]);
    let deep = nested_markers(&setup);
    std::fs::write(setup.home.join("outer/inner/.here"), "").unwrap();
    let d = setup.daemon();
    assert_eq!(
        get_at(&d, &deep, &["rep"]),
        Some(json!({"a": "inner-a"})),
        "from a rooted caller: the nearest instance, and the outer one does not fill in"
    );
    assert_eq!(
        get_at(&d, &deep, &["nores"]),
        Some(json!({"a": "inner-a"})),
        "a provider with no Sources calls from the directory read, not a layer's root"
    );
}

#[test]
fn a_called_sources_own_failure_is_the_callers_miss() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.bad.sources.t]
data = "{{ exec('exit 3') }}"
result.v = "{{ data }}"
return = "{{ result | failback(1, '1h') }}"

[daemon.providers.viadata.sources.s]
data = "{{ daemon.providers.bad.sources.t().v }}"
result.x = "{{ data }}"

[daemon.providers.viareturn]
return = "{{ result | merge(daemon.providers.bad.sources.t()) }}"

[daemon.providers.viareturn.sources.own]
data = "{{ 2 }}"
result.mine = "{{ data }}"
"#,
    )]);
    let d = setup.daemon();
    for _ in 0..2 {
        let out = d
            .comb(&d.home)
            .args(["get", "-f", "json", "viadata.x"])
            .output()
            .expect("run comb get");
        assert!(out.status.success() && out.stdout == b"null\n", "{out:?}");
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("exit"),
            "a miss, not the callee's error: {out:?}"
        );
        assert_eq!(
            get(&d, "viareturn"),
            Some(json!({"mine": 2})),
            "a line whose call finds nothing answers without it"
        );
    }
}

#[test]
fn a_call_goes_on_up_the_chain_past_an_instance_that_fails() {
    let setup = Setup::new(&[(
        "config.toml",
        &format!(
            r#"{}
[daemon.providers.rep.sources.c]
root = "{{{{ lup('.here') | dirname }}}}"
data = "{{{{ daemon.providers.demo.sources.s() }}}}"
result = "{{{{ data }}}}"
"#,
            vals_config("demo", false)
        ),
    )]);
    let deep = nested_markers(&setup);
    std::fs::write(setup.home.join("outer/inner/.here"), "").unwrap();
    // The nearer instance of the called Source cannot run.
    std::fs::write(setup.home.join("outer/inner/vals.json"), "not json").unwrap();
    let d = setup.daemon();
    let direct = get_at(&d, &deep, &["demo"]);
    assert_eq!(
        direct,
        Some(json!({"a": "outer-a", "b": "outer-b"})),
        "a direct read carries on to the outer instance"
    );
    assert_eq!(
        get_at(&d, &deep, &["rep"]),
        direct,
        "a call answers as the direct read from the same directory does"
    );
}

#[test]
fn get_force_runs_the_sources_a_providers_lines_call_again() {
    let setup = Setup::new(&[]);
    let count = setup.home.join("tick.count");
    setup.write(
        "config.toml",
        &format!(
            r#"
[daemon.providers.tick.sources.t]
data = "{{{{ exec(['sh', '-c', 'echo run >> {}; wc -l < {}']) | trim }}}}"
result.n = "{{{{ data }}}}"

[daemon.providers.seen]
return = "{{{{ daemon.providers.tick.sources.t() }}}}"
"#,
            count.display(),
            count.display()
        ),
    );
    let d = setup.daemon();
    assert_eq!(get(&d, "seen"), Some(json!({"n": "1"})));
    assert_eq!(get(&d, "seen"), Some(json!({"n": "1"})), "warm: not run");
    let out = d
        .comb(&d.home)
        .args(["get", "--force", "-f", "json", "seen"])
        .output()
        .expect("run comb get --force");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap(),
        json!({"n": "2"}),
        "--force re-ran the called Source"
    );
}

/// A Source that appends a line to `count` each time it runs, and nothing else.
fn tally_source(count: &Path) -> String {
    format!(
        "data = \"{{{{ exec(['sh', '-c', 'echo run >> {c} ; echo 1']) | trim }}}}\"\nresult.v = \"{{{{ data }}}}\"\n",
        c = count.display()
    )
}

/// Open a `watch` of `key` on a connection of its own and read its first
/// frame: when it has come, the daemon has handled the request.
fn watching(d: &TestDaemon, key: &str) -> std::os::unix::net::UnixStream {
    use std::io::{BufRead, BufReader, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(&d.socket.path).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    writeln!(stream, "{}", json!({"op": "watch", "key": key})).unwrap();
    let mut first = String::new();
    BufReader::new(stream.try_clone().unwrap())
        .read_line(&mut first)
        .expect("the first frame of a watch");
    assert!(!first.is_empty(), "the watch of {key} answered");
    stream
}

fn refresh_ok(d: &TestDaemon, key: &str) -> bool {
    d.comb(&d.home)
        .args(["refresh", key])
        .output()
        .expect("run comb refresh")
        .status
        .success()
}

#[test]
fn a_disabled_provider_is_a_miss() {
    let setup = Setup::new(&[]);
    let off_count = setup.home.join("off.count");
    let on_count = setup.home.join("on.count");
    setup.write(
        "config.toml",
        &format!(
            r#"
[daemon.providers.off]
enabled = false

[daemon.providers.off.sources.s]
{}
[daemon.providers.on.sources.s]
{}"#,
            tally_source(&off_count),
            tally_source(&on_count),
        ),
    );
    let d = setup.daemon();

    assert_eq!(get(&d, "off"), None, "the provider is a miss");
    assert_eq!(get(&d, "off.v"), None, "and so is a field of it");
    let _watch = watching(&d, "off");
    assert!(
        !refresh_ok(&d, "off"),
        "a disabled provider has nothing to refresh"
    );

    // A read and a refresh of a provider that is enabled have gone through the
    // same daemon and the same scheduler: what they did, the disabled
    // provider's requests had the chance to do first.
    assert_eq!(get(&d, "on.v"), Some(json!("1")));
    assert!(refresh_ok(&d, "on"));
    poll_until(
        "the enabled Source",
        "re-run by the refresh",
        Duration::from_secs(20),
        Duration::from_millis(50),
        || runs(&on_count),
        |n| *n >= 2,
    );

    assert_eq!(runs(&off_count), 0, "no Source of a disabled provider ran");
    assert_eq!(
        warm_instances(&d, "off"),
        Vec::<(String, Option<String>)>::new(),
        "none is kept warm"
    );
    assert!(
        !warm_instances(&d, "on").is_empty(),
        "the enabled one is, so the check above can tell"
    );
}

#[test]
fn a_later_layer_can_re_enable_a_provider() {
    let setup = Setup::new(&[]);
    let count = setup.home.join("p.count");
    let source = tally_source(&count);
    setup.write(
        "config.toml",
        &format!(
            "[daemon.providers.p]\nenabled = false\n\n[daemon.providers.p.sources.s]\n{source}"
        ),
    );
    let d = setup.daemon();
    assert_eq!(get(&d, "p"), None);
    assert_eq!(runs(&count), 0);
    drop(d);

    setup.write(
        "conf.d/10-back.toml",
        "[daemon.providers.p]\nenabled = true\n",
    );
    let d = setup.daemon();
    assert_eq!(get(&d, "p"), Some(json!({"v": "1"})));
    assert_eq!(runs(&count), 1);
    drop(d);

    // And the other way round: a later layer disables what an earlier enabled.
    setup.write(
        "config.toml",
        &format!(
            "[daemon.providers.p]\nenabled = true\n\n[daemon.providers.p.sources.s]\n{source}"
        ),
    );
    setup.write(
        "conf.d/10-back.toml",
        "[daemon.providers.p]\nenabled = false\n",
    );
    let d = setup.daemon();
    assert_eq!(get(&d, "p"), None);
    assert_eq!(runs(&count), 1, "the later layer's false is the value");
}

#[test]
fn a_built_in_provider_can_be_disabled() {
    assert!(
        crate::common::git::has_git(),
        "this test needs git on $PATH"
    );
    let repo = crate::common::git::GitRepoFixture::new();
    let inside = repo.path().canonicalize().unwrap();
    let in_repo = |p: &String| p.starts_with(inside.to_str().unwrap());

    // The control: with git enabled the same read keeps git warm and watches
    // the repository, so the absence below means something.
    let on = Setup::new(&[]);
    let d = on.daemon();
    assert_eq!(get_at(&d, &inside, &["git.branch"]), Some(json!("main")));
    assert!(!warm_instances(&d, "git").is_empty(), "git is kept warm");
    poll_until(
        "the enabled daemon",
        "watching the repository",
        Duration::from_secs(20),
        Duration::from_millis(50),
        || watched(&d),
        |paths| paths.iter().any(in_repo),
    );
    drop(d);

    let off = Setup::new(&[("config.toml", "[daemon.providers.git]\nenabled = false\n")]);
    let d = off.daemon();
    assert_eq!(get_at(&d, &inside, &["git.branch"]), None);
    assert_eq!(get_at(&d, &inside, &["git"]), None);
    assert!(
        !d.comb(&inside)
            .args(["refresh", "git"])
            .output()
            .unwrap()
            .status
            .success()
    );
    // A positive event after the requests above: another provider's read.
    assert!(get_at(&d, &inside, &["hostname"]).is_some());
    assert_eq!(
        warm_instances(&d, "git"),
        Vec::<(String, Option<String>)>::new()
    );
    assert!(
        !watched(&d).iter().any(in_repo),
        "nothing watches the repository: {:?}",
        watched(&d)
    );
}

#[test]
fn a_disabled_template_only_provider_is_a_miss_but_its_facet_reads() {
    // aws is template-only: the shipped defaults give it templates and no
    // daemon Source.
    let setup = Setup::new(&[("config.toml", "[daemon.providers.aws]\nenabled = false\n")]);
    let d = setup.daemon();
    assert_eq!(get(&d, "aws"), None, "no template half rides along");
    assert_eq!(get(&d, "aws.region"), None);
    let facet = get(&d, "aws:template").expect("the facet is still seeded");
    assert!(facet.get("region").is_some(), "{facet}");

    let on = Setup::new(&[]);
    let d = on.daemon();
    assert!(
        get(&d, "aws").is_some(),
        "control: enabled, the templates render"
    );
}

#[test]
fn a_disabled_provider_with_a_result_of_its_own_is_still_disabled() {
    let setup = Setup::new(&[]);
    let count = setup.home.join("q.count");
    setup.write(
        "config.toml",
        &format!(
            r#"
[daemon.providers.q]
enabled = false
result = "{{{{ merge(sources) }}}}"

[daemon.providers.q.sources.s]
{}"#,
            tally_source(&count)
        ),
    );
    let d = setup.daemon();
    assert_eq!(get(&d, "q"), None);
    assert_eq!(get(&d, "q.v"), None);
    assert_eq!(runs(&count), 0);
}

#[test]
fn a_call_to_a_disabled_providers_source_is_a_miss_that_runs_nothing() {
    let setup = Setup::new(&[]);
    let count = setup.home.join("off.count");
    setup.write(
        "config.toml",
        &format!(
            r#"
[daemon.providers.off]
enabled = false

[daemon.providers.off.sources.s]
{}
[daemon.providers.caller]
result = "{{{{ merge(sources) | merge(daemon.providers.off.sources.s()) }}}}"

[daemon.providers.caller.sources.own]
data = "{{{{ 2 }}}}"
result.mine = "{{{{ data }}}}"

[daemon.providers.viadata.sources.own]
data = "{{{{ 'miss' if daemon.providers.off.sources.s() is none else 'hit' }}}}"
result.seen = "{{{{ data }}}}"
"#,
            tally_source(&count)
        ),
    );
    let d = setup.daemon();
    assert_eq!(
        get(&d, "caller"),
        Some(json!({"mine": 2})),
        "the call gave a miss, which the merge skips"
    );
    assert_eq!(
        get(&d, "viadata"),
        Some(json!({"seen": "miss"})),
        "so does a Source's data line"
    );
    assert!(refresh_ok(&d, "caller"));
    let _watch = watching(&d, "caller");
    assert_eq!(runs(&count), 0, "the call did not run the Source");
    assert_eq!(
        warm_instances(&d, "off"),
        Vec::<(String, Option<String>)>::new()
    );
}

#[test]
fn a_disabled_provider_drops_its_template_half() {
    let setup = Setup::new(&[(
        "config.toml",
        r#"
[daemon.providers.off]
enabled = false

[client.providers.off.template]
label = "{{ 'static' }}"
"#,
    )]);
    let d = setup.daemon();
    // `enabled = false` is `result = "{{ {} }}"` and `return = "{{ result }}"`:
    // no data, and no template half to render client-side.
    assert_eq!(get(&d, "off"), None);
    assert_eq!(get(&d, "off.label"), None);
    assert_eq!(
        get(&d, "off:template"),
        Some(json!({"label": "{{ 'static' }}"})),
        "the facet is data, and is read by its own name"
    );
}

#[test]
fn a_put_under_a_disabled_provider_is_still_read() {
    let setup = Setup::new(&[(
        "config.toml",
        "[daemon.providers.off]\nenabled = false\n\n[client.providers.off.template]\ny = \"{{ 1 }}\"\n",
    )]);
    let d = setup.daemon();
    d.comb(&d.home)
        .args(["put", "off.x", "\"kept\""])
        .assert()
        .success();
    assert_eq!(
        get(&d, "off"),
        Some(json!({"x": "kept"})),
        "a put is not a Source's data, and disabling is only about Sources"
    );
}

#[test]
fn a_return_line_call_to_a_disabled_source_is_a_miss_that_runs_nothing() {
    let setup = Setup::new(&[]);
    let count = setup.home.join("off.count");
    setup.write(
        "config.toml",
        &format!(
            r#"
[daemon.providers.off]
enabled = false

[daemon.providers.off.sources.s]
{}
[daemon.providers.caller]
return = "{{{{ result | merge(daemon.providers.off.sources.s()) }}}}"

[daemon.providers.caller.sources.own]
data = "{{{{ 2 }}}}"
result.mine = "{{{{ data }}}}"
"#,
            tally_source(&count)
        ),
    );
    let d = setup.daemon();
    assert_eq!(get(&d, "caller"), Some(json!({"mine": 2})));
    let _watch = watching(&d, "caller");
    assert_eq!(runs(&count), 0);
    assert!(warm_instances(&d, "off").is_empty());
}

/// `comb <args>` from `d`'s home: whether it succeeded, and its stderr.
fn comb_status(d: &TestDaemon, args: &[&str]) -> (bool, String) {
    let out = d.comb(&d.home).args(args).output().expect("run comb");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn refresh_wait_and_force_of_a_disabled_provider() {
    let setup = Setup::new(&[]);
    let off_count = setup.home.join("off.count");
    setup.write(
        "config.toml",
        &format!(
            "[daemon.providers.off]\nenabled = false\n\n[daemon.providers.off.sources.s]\n{}",
            tally_source(&off_count)
        ),
    );
    let d = setup.daemon();
    let (ok, err) = comb_status(&d, &["refresh", "off"]);
    assert!(
        !ok && err.contains("nothing to refresh: off is disabled"),
        "{err}"
    );
    let (ok, err) = comb_status(&d, &["get", "--force", "off"]);
    assert!(
        !ok && err.contains("cannot --force disabled provider 'off'"),
        "an error like --force on a static namespace, not a silent miss: {err}"
    );
    let out = d
        .comb(&d.home)
        .args(["get", "--wait", "-f", "json", "off"])
        .output()
        .unwrap();
    assert!(
        out.status.success() && out.stdout == b"null\n",
        "--wait is a miss, not a run: {out:?}"
    );
    assert_eq!(runs(&off_count), 0);
}

#[test]
fn introspect_providers_marks_a_disabled_provider() {
    let setup = Setup::new(&[(
        "config.toml",
        "[daemon.providers.hostname]\nenabled = false\n",
    )]);
    let d = setup.daemon();
    let providers = introspect(&d, "providers");
    let row = |name: &str| {
        providers["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("{name} listed"))
            .clone()
    };
    let off = row("hostname");
    assert_eq!(off["disabled"], json!(true));
    assert_eq!(off["invalidation"], json!("disabled"), "{off}");
    assert!(
        off["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["invalidation"] == "disabled"),
        "{off}"
    );
    assert_eq!(row("load")["disabled"], json!(false));
}
