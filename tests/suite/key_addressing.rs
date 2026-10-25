//! Key addressing: which forms of a key resolve to a value and which miss.
//!
//! `provider.source` and `provider.source.field` key addressing has been
//! removed: a source name is metadata on the record, never part of an
//! address (`docs/canon/provider_source.md` "Record"). `parse_key` in
//! `src/query.rs` no longer disambiguates a second segment against the
//! registry — it is always the field. These tests pin the resulting wire
//! behaviour through a real socket, mirroring `tests/suite/store.rs`/
//! `tests/suite/facets.rs`'s raw-socket style, against a real git repo (`git` has
//! a `refs` source with an `ahead` field, so `git.refs`/`git.refs.ahead` is
//! exactly the collision this change removes). This is an area file for
//! key-addressing behaviour generally — later key-addressing tests join it
//! here rather than starting a new file.

use crate::common::git::GitRepoFixture;
use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn setup() -> (
    TempDir,
    std::path::PathBuf,
    Arc<Cache>,
    Arc<ProviderRegistry>,
    Arc<beachcomber::watcher_registry::WatcherRegistry>,
) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());
    (tmp, sock, cache, registry, watchers)
}

async fn send_recv(stream: &mut UnixStream, request: &serde_json::Value) -> Response {
    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(&mut *stream);
    let mut resp_line = String::new();
    reader.read_line(&mut resp_line).await.unwrap();
    serde_json::from_str(&resp_line).unwrap()
}

/// The documented shape of a miss is `{"ok":true}` with every other field
/// absent — not just an absent `data`.
fn assert_miss(resp: &Response, context: &str) {
    assert!(
        resp.ok,
        "{context}: a miss is still ok, got error {:?}",
        resp.error
    );
    assert!(
        resp.data.is_none(),
        "{context}: data should be absent, got {:?}",
        resp.data
    );
    assert!(
        resp.age_ms.is_none(),
        "{context}: age_ms should be absent, got {:?}",
        resp.age_ms
    );
    assert!(
        resp.stale.is_none(),
        "{context}: stale should be absent, got {:?}",
        resp.stale
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_name_as_key_segment_misses_but_the_field_still_hits() {
    if !crate::common::git::has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let repo = GitRepoFixture::new();
    let path = repo.path_str();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // "refs" is a source name, not a field — git.refs is a miss, not an
    // error: an unknown field is a miss like any other.
    let resp = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "git.refs", "path": path}),
    )
    .await;
    assert_miss(&resp, "git.refs before warming");

    // "stash" is a real field of the "refs" source, but addressing it through
    // the (removed) source-field form also just misses now.
    let resp = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "git.refs.stash", "path": path}),
    )
    .await;
    assert_miss(&resp, "git.refs.stash before warming");

    // git.stash is the field's one real address, and it still hits — this
    // also warms the "refs" source in the cache.
    let resp = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "git.stash", "path": path}),
    )
    .await;
    assert!(resp.ok, "git.stash should hit: {:?}", resp.error);
    assert!(
        resp.data.as_ref().is_some_and(|d| d.is_number()),
        "git.stash should be numeric, got {:?}",
        resp.data
    );

    // A source name is not a key even once the source is warm: "refs" is
    // still not a field, regardless of whether its data now sits in cache.
    let resp = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "git.refs", "path": path}),
    )
    .await;
    assert_miss(&resp, "git.refs after warming refs");

    let resp = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "git.refs.stash", "path": path}),
    )
    .await;
    assert_miss(&resp, "git.refs.stash after warming refs");

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whole_provider_flattening_has_no_key_named_for_a_source() {
    if !crate::common::git::has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let repo = GitRepoFixture::new();
    let path = repo.path_str();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let resp = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "git", "path": path}),
    )
    .await;
    assert!(resp.ok, "whole-provider get should hit: {:?}", resp.error);
    let data = resp.data.expect("git provider should have data");
    let obj = data.as_object().expect("object");
    assert!(
        obj.contains_key("stash"),
        "flattened fields include stash, got keys: {:?}",
        obj.keys().collect::<Vec<_>>()
    );
    assert!(
        !obj.contains_key("refs"),
        "a source name is never a key in the flattened object, got keys: {:?}",
        obj.keys().collect::<Vec<_>>()
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_on_a_source_name_key_opens_and_misses() {
    if !crate::common::git::has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let repo = GitRepoFixture::new();
    let path = repo.path_str();

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let request = serde_json::json!({"op": "watch", "key": "git.refs", "path": path}).to_string();
    writer
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut line = String::new();
    let result = tokio::time::timeout(Duration::from_millis(500), reader.read_line(&mut line))
        .await
        .expect("timed out waiting for the initial watch frame")
        .unwrap();
    assert!(result > 0, "expected a response line");

    let resp: Response = serde_json::from_str(&line).unwrap();
    assert_miss(&resp, "watch git.refs initial frame");

    handle.abort();
}

// ── The key grammar through `comb get` ───────────────────────────────────────
//
// One grammar (`libbeachcomber::key`) for every key: `.seg`, `[n]` and
// `['…']`, jinja's own spellings, and a segment below the head may hold `:`,
// `/`, `@` and spaces bare.

mod grammar {
    use crate::common::daemon::TestDaemon;
    use crate::common::socket::IsolatedSocket;
    use std::path::{Path, PathBuf};

    fn project(d: &TestDaemon) -> PathBuf {
        let dir = d.home.join("ws").join("project");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `comb get --path <cwd> <args…>` as text, which must succeed. `--path`
    /// so no key here is taken for a trailing directory.
    fn get(d: &TestDaemon, cwd: &Path, args: &[&str]) -> String {
        let out = d
            .comb(cwd)
            .arg("get")
            .arg("--path")
            .arg(cwd)
            .args(args)
            .assert()
            .success();
        String::from_utf8(out.get_output().stdout.clone()).unwrap()
    }

    fn put(d: &TestDaemon, cwd: &Path, key: &str, data: &str) {
        d.comb(cwd)
            .args(["put", "--path"])
            .arg(cwd)
            .args([key, data])
            .assert()
            .success();
    }

    #[test]
    fn every_jinja_spelling_of_an_address_reads_the_same_node() {
        let d = TestDaemon::spawn();
        let cwd = project(&d);
        put(
            &d,
            &cwd,
            "p",
            r#"{"list":[10,20],"o":{"a b":1,"it's":2,"x:y":3,"s/t":4,"say \"hi\"":5}}"#,
        );

        for (key, want) in [
            ("p.list.0", "10"),
            ("p.list[1]", "20"),
            ("p['list'][1]", "20"),
            ("p.o.x:y", "3"),
            ("p['o']['x:y']", "3"),
            (r#"p["o"]["x:y"]"#, "3"),
            ("p.o.s/t", "4"),
            ("p.o['a b']", "1"),
            ("p.o.a b", "1"),
            (r#"p["o"]["it's"]"#, "2"),
            (r"p.o['it\'s']", "2"),
            (r#"p.o['say "hi"']"#, "5"),
            (r#"p.o["say \"hi\""]"#, "5"),
        ] {
            assert_eq!(get(&d, &cwd, &[key]), want, "{key}");
        }
    }

    /// `env`, `comb`, `_` and `*` mean something of their own, so no `put`
    /// may take one as a namespace and no read may name one as a provider:
    /// one rule and one message on every op. `env.X` still reads the shell,
    /// and `*:cache._.decay` is still the decay of every provider.
    #[test]
    fn a_reserved_word_is_no_namespace() {
        let d = TestDaemon::spawn();
        let cwd = project(&d);

        for (key, word) in [
            ("env.FOO", "env"),
            ("comb.x", "comb"),
            ("_", "_"),
            ("*", "*"),
        ] {
            d.comb(&cwd)
                .args(["put", key, r#"{"a":1}"#])
                .assert()
                .code(2)
                .stderr(predicates::str::contains(format!("'{word}' is reserved")));
        }
        d.comb(&cwd)
            .args(["get", "comb.x"])
            .assert()
            .code(2)
            .stderr(predicates::str::contains("'comb' is reserved"));
        d.comb(&cwd)
            .args(["refresh", "_"])
            .assert()
            .code(2)
            .stderr(predicates::str::contains("'_' is reserved"));

        let home = d.comb(&cwd).args(["get", "env.HOME"]).assert().success();
        assert!(
            !home.get_output().stdout.is_empty(),
            "env.X reads the shell"
        );
        d.comb(&cwd)
            .args(["put", "*:cache._.decay", "3"])
            .assert()
            .success();
    }

    #[test]
    fn a_colon_below_the_head_is_an_ordinary_miss_not_an_error() {
        let d = TestDaemon::spawn();
        let cwd = project(&d);
        put(&d, &cwd, "p", r#"{"a":1}"#);
        assert_eq!(get(&d, &cwd, &["p.a:age"]), "");
    }

    #[test]
    fn a_malformed_key_is_invalid() {
        let d = TestDaemon::spawn();
        let cwd = project(&d);
        for key in ["p..a", "p['a", "p[01]"] {
            d.comb(&cwd)
                .args(["get", "--path"])
                .arg(&cwd)
                .arg(key)
                .assert()
                .code(2)
                .stderr(predicates::str::contains(format!("invalid key: {key}")));
        }
    }

    /// Anything comb can read it can write: a name holding a `.` is a quoted
    /// segment in a `put` key and a plain key in its data, and reads back the
    /// way it was written.
    #[test]
    fn a_name_holding_a_dot_is_written_as_it_is_read() {
        let d = TestDaemon::spawn();
        let cwd = project(&d);

        put(&d, &cwd, "p['a.b']", "1");
        put(&d, &cwd, "p.o", r#"{"c.d": 2}"#);
        assert_eq!(get(&d, &cwd, &["p['a.b']"]), "1");
        assert_eq!(get(&d, &cwd, &["p.o['c.d']"]), "2");
        assert_eq!(
            get(&d, &cwd, &["p"]),
            "['a.b']=1\no['c.d']=2\n",
            "printed as keys it can be read back by"
        );

        put(&d, &cwd, "p['a.b']", "null");
        assert_eq!(get(&d, &cwd, &["p['a.b']"]), "");
    }

    /// The same name in configuration: a quoted TOML key, in a template and
    /// in a Source's `result`, reads as the quoted segment.
    #[test]
    fn a_configured_name_holding_a_dot_reads_as_a_quoted_segment() {
        let socket = IsolatedSocket::new();
        let home = socket.dir.path().join("home");
        let xdg = socket.dir.path().join("xdg");
        std::fs::create_dir_all(xdg.join("beachcomber")).unwrap();
        std::fs::write(
            xdg.join("beachcomber").join("config.toml"),
            r#"
[client.providers.t]
template."a.b" = "{{ 'tpl' }}"

[daemon.providers.s.sources.main]
data = "{{ 'src' }}"
result."c.d" = "{{ data }}"
"#,
        )
        .unwrap();
        let d = TestDaemon::spawn_in(socket, home, xdg);
        let cwd = project(&d);

        assert_eq!(get(&d, &cwd, &["t['a.b']"]), "tpl");
        assert_eq!(get(&d, &cwd, &["s['c.d']"]), "src");
        assert_eq!(get(&d, &cwd, &["s"]), "['c.d']=src\n");
    }

    /// A daemon whose `PATH` leads with a fake `mise` that reports two tools
    /// for `project`: one named with a colon, one with dots and slashes.
    fn mise_daemon() -> (TestDaemon, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let socket = IsolatedSocket::new();
        let home = socket.dir.path().join("home");
        let xdg = socket.dir.path().join("xdg");
        let cwd = home.join("ws").join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("mise.toml"), "").unwrap();
        let config = cwd.join("mise.toml");
        let config = config.display();
        let bin = socket.dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let mise = bin.join("mise");
        std::fs::write(
            &mise,
            format!(
                "#!/bin/sh\ncat <<'JSON'\n{{\"cargo:x\":[{{\"version\":\"1.0\",\"source\":{{\"path\":\"{config}\"}}}}],\
                 \"go:golang.org/x/tools/gopls\":[{{\"version\":\"0.16.0\",\"source\":{{\"path\":\"{config}\"}}}}]}}\nJSON\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&mise, std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: nextest runs each test in its own process; the daemon below
        // inherits this `PATH`, so its `mise` is the fake one.
        unsafe {
            std::env::set_var(
                "PATH",
                format!(
                    "{}:{}",
                    bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        }
        (TestDaemon::spawn_in(socket, home, xdg), cwd)
    }

    #[test]
    fn a_mise_tool_named_with_a_colon_or_dots_is_readable_alone() {
        let (d, cwd) = mise_daemon();

        assert_eq!(get(&d, &cwd, &["mise.cargo:x"]), "1.0");
        assert_eq!(
            get(&d, &cwd, &["mise['go:golang.org/x/tools/gopls']"]),
            "0.16.0"
        );
        // Unquoted, the dots are segments, and that address holds nothing.
        assert_eq!(get(&d, &cwd, &["mise.go:golang.org/x/tools/gopls"]), "");
    }

    #[test]
    fn every_address_comb_prints_can_be_typed_back() {
        let (d, cwd) = mise_daemon();

        let text = get(&d, &cwd, &["mise"]);
        assert!(
            text.lines().any(|l| l == "cargo:x=1.0"),
            "a colon needs no quoting: {text}"
        );
        assert!(
            text.lines()
                .any(|l| l == "['go:golang.org/x/tools/gopls']=0.16.0"),
            "a name holding a dot prints quoted: {text}"
        );

        let out = d
            .comb(&cwd)
            .args(["status", "-f", "json", "--filter", "provider=mise"])
            .assert()
            .success();
        let rows: Vec<serde_json::Value> = String::from_utf8_lossy(&out.get_output().stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let fields: Vec<&str> = rows.iter().filter_map(|r| r["field"].as_str()).collect();
        assert!(
            fields.contains(&"['go:golang.org/x/tools/gopls']"),
            "status names the row by its key: {fields:?}"
        );
        for field in fields {
            let key = libbeachcomber::key::join("mise", field);
            assert_ne!(get(&d, &cwd, &[&key]), "", "{key} reads back");
        }
    }

    /// A key holding a `/` is a key, alone or last: only an argument that
    /// starts with `/`, `~` or `.` is the directory.
    #[test]
    fn a_key_holding_a_slash_is_never_taken_for_the_directory() {
        let (d, cwd) = mise_daemon();
        let gopls = "mise['go:golang.org/x/tools/gopls']";

        let alone = d.comb(&cwd).args(["get", gopls]).assert().success();
        assert_eq!(
            String::from_utf8_lossy(&alone.get_output().stdout),
            "0.16.0"
        );

        let last = d
            .comb(&cwd)
            .args(["get", "mise.cargo:x", gopls])
            .assert()
            .success();
        assert_eq!(
            String::from_utf8_lossy(&last.get_output().stdout),
            "1.0\n0.16.0\n"
        );
    }

    /// A quoted `~` the shell did not expand is still `$HOME`.
    #[test]
    fn a_quoted_tilde_directory_is_home() {
        let d = TestDaemon::spawn();
        let cwd = project(&d);
        put(&d, &cwd, "p", r#"{"a":1}"#);

        let elsewhere = d.home.join("ws");
        let out = d
            .comb(&elsewhere)
            .args(["get", "p.a", "~/ws/project"])
            .assert()
            .success();
        assert_eq!(String::from_utf8_lossy(&out.get_output().stdout), "1");
    }

    /// `--tree` finds the layers of a row whose field is quoted: the field
    /// is an address, walked by its segments, not split on its dots.
    #[test]
    fn tree_finds_the_layers_of_a_quoted_field() {
        let (d, cwd) = mise_daemon();
        get(&d, &cwd, &["mise"]);

        let out = d
            .comb(&cwd)
            .args([
                "status",
                "--tree",
                "-f",
                "json",
                "--filter",
                "provider=mise",
            ])
            .assert()
            .success();
        let rows: Vec<serde_json::Value> = String::from_utf8_lossy(&out.get_output().stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let gopls = rows
            .iter()
            .find(|r| r["field"] == "['go:golang.org/x/tools/gopls']")
            .unwrap_or_else(|| panic!("a gopls row: {rows:#?}"));
        let layers = gopls["layers"].as_array().expect("layers");
        assert!(
            layers
                .iter()
                .any(|l| l["answered"] == true && l["value"] == "0.16.0"),
            "the answering layer: {layers:#?}"
        );

        // On the wire, the `layers` tree is keyed by the stored names, so a
        // name holding dots is one key, not a path of three.
        use std::io::{BufRead, BufReader, Write};
        let mut stream = std::os::unix::net::UnixStream::connect(&d.socket.path).unwrap();
        let request = serde_json::json!({"op": "layers", "key": "mise", "path": cwd});
        writeln!(stream, "{request}").unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(
            resp["data"]["go:golang.org/x/tools/gopls"].is_array(),
            "one key for the dotted name: {resp:#}"
        );
    }

    /// A negative index counts from the end, in a key as in `pluck`: the two
    /// read one grammar, so a path one accepts the other does too.
    #[test]
    fn a_negative_index_reads_from_the_end_in_get_and_in_pluck() {
        let d = TestDaemon::spawn();
        let cwd = project(&d);
        put(&d, &cwd, "p", r#"{"l":[1,2,3]}"#);

        assert_eq!(get(&d, &cwd, &["p.l[-1]"]), "3");
        assert_eq!(get(&d, &cwd, &["p.l.-3"]), "1");
        assert_eq!(get(&d, &cwd, &["p.l[-4]"]), "");

        let eval = |src: &str| {
            let out = d.comb(&cwd).args(["eval", src]).output().unwrap();
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        };
        assert_eq!(eval("{{ comb.p.l | pluck('[-1]') }}").1, "3");
        let (code, _, err) = eval("{{ comb.p.l | pluck('[01]') }}");
        assert_eq!(
            code,
            Some(2),
            "a key comb get refuses, pluck refuses: {err}"
        );
        assert!(err.contains("pluck()"), "{err}");
    }

    /// The `:cache` facet answers every leaf of a node, a leaf whose name
    /// holds a `.` included.
    #[test]
    fn the_cache_facet_answers_a_leaf_whose_name_holds_a_dot() {
        let d = TestDaemon::spawn();
        let cwd = project(&d);
        put(&d, &cwd, "p", r#"{"a.b":5,"o":{"x":1}}"#);

        let out = d
            .comb(&cwd)
            .args(["get", "-f", "json", "--path"])
            .arg(&cwd)
            .arg("p:cache")
            .assert()
            .success();
        let tree: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(tree["a.b"]["source"], "put", "{tree:#}");
        assert_eq!(tree["o"]["x"]["source"], "put", "{tree:#}");
    }

    /// `put` names a provider by the rule configuration does.
    #[test]
    fn put_refuses_the_names_config_refuses() {
        let d = TestDaemon::spawn();
        let cwd = project(&d);
        for head in ["<x>", "a*b"] {
            d.comb(&cwd)
                .args(["put", &format!("{head}.v"), "1"])
                .assert()
                .code(2)
                .stderr(predicates::str::contains(format!(
                    "invalid provider name '{head}'"
                )));
        }
        put(&d, &cwd, "a-b", r#"{"v":1}"#);
        assert_eq!(get(&d, &cwd, &["a-b.v"]), "1");
    }
}
