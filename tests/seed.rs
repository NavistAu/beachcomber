//! Facet tables in configuration seed their namespaces at daemon start, one
//! `config:<file>` label per file, and re-seed in place when a file changes
//! (spec §3.1, §3.2, §3.9).
//!
//! Each test owns its `$HOME` and its `$XDG_CONFIG_HOME`: nextest runs every
//! test in its own process, so `set_var` here is process-local — the same
//! assumption `tests/path_chain.rs` makes.

mod common;

use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::Value as CacheValue;
use beachcomber::provider::registry::ProviderRegistry;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

// ── fixture ──────────────────────────────────────────────────────────────────

struct Env {
    _home: TempDir,
    _cfg: TempDir,
    _sock: TempDir,
    cfg_dir: PathBuf,
    sock: PathBuf,
}

impl Env {
    /// Point `$HOME` and `$XDG_CONFIG_HOME` at fresh temp roots, then write
    /// `config.toml` and the named `conf.d` drop-ins.
    fn new(main: &str, drop_ins: &[(&str, &str)]) -> Env {
        let home = TempDir::new().unwrap();
        // SAFETY: nextest gives every test its own process, so no other thread
        // is reading the environment concurrently.
        unsafe { std::env::set_var("HOME", home.path().canonicalize().unwrap()) };
        let cfg = TempDir::new().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", cfg.path()) };

        let cfg_dir = cfg.path().join("beachcomber");
        std::fs::create_dir_all(cfg_dir.join("conf.d")).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), main).unwrap();
        for (name, body) in drop_ins {
            std::fs::write(cfg_dir.join("conf.d").join(name), body).unwrap();
        }

        let sock_dir = TempDir::new().unwrap();
        let sock = sock_dir.path().join("t.sock");
        Env {
            _home: home,
            _cfg: cfg,
            _sock: sock_dir,
            cfg_dir,
            sock,
        }
    }

    fn start(
        &self,
    ) -> (
        tokio::task::JoinHandle<()>,
        beachcomber::daemon::DaemonReady,
    ) {
        beachcomber::daemon::start_in_process_polling(self.sock.clone(), Config::load())
    }

    /// Start and wait for the socket; panics if the daemon refuses to start.
    async fn serve(&self) -> tokio::task::JoinHandle<()> {
        let (handle, ready) = self.start();
        ready.await.expect("daemon ready");
        handle
    }

    async fn connect(&self) -> UnixStream {
        UnixStream::connect(&self.sock).await.unwrap()
    }

    fn label(&self, rel: &str) -> String {
        format!("config:{}", self.cfg_dir.join(rel).display())
    }

    fn drop_in(&self, name: &str) -> PathBuf {
        self.cfg_dir.join("conf.d").join(name)
    }
}

/// Rewrite a config file and stamp its mtime into the future, so the watch's
/// poll sees the change whatever the clock's granularity (the trick
/// `tests/config_self_watch.rs` uses).
fn rewrite(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(2))
        .unwrap();
}

async fn send_recv(stream: &mut UnixStream, request: serde_json::Value) -> serde_json::Value {
    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(&mut *stream);
    let mut resp = String::new();
    reader.read_line(&mut resp).await.unwrap();
    serde_json::from_str(&resp).unwrap()
}

fn get(key: &str) -> serde_json::Value {
    json!({"op": "get", "key": key})
}

/// The `data` half of a `get`, or `Null` on a miss.
async fn data(stream: &mut UnixStream, key: &str) -> serde_json::Value {
    let resp = send_recv(stream, get(key)).await;
    assert_eq!(resp["ok"], json!(true), "get {key}: {resp}");
    resp.get("data").cloned().unwrap_or(serde_json::Value::Null)
}

/// The `template` half of a `get`, or `Null` when none is attached.
async fn template(stream: &mut UnixStream, key: &str) -> serde_json::Value {
    let resp = send_recv(stream, get(key)).await;
    assert_eq!(resp["ok"], json!(true), "get {key}: {resp}");
    resp.get("template")
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// `(source, value, kind)` for one `comb status` row, or `None` if absent.
async fn row(
    stream: &mut UnixStream,
    provider: &str,
    field: &str,
) -> Option<(String, serde_json::Value, serde_json::Value)> {
    let resp = send_recv(stream, json!({"op": "status"})).await;
    resp["data"].as_array()?.iter().find_map(|r| {
        (r["provider"] == json!(provider) && r["field"] == json!(field)).then(|| {
            (
                r["source"].as_str().unwrap_or_default().to_string(),
                r["value"].clone(),
                r["kind"].clone(),
            )
        })
    })
}

// ── (a) one label per file ───────────────────────────────────────────────────

const MAIN_TEMPLATE: &str = r#"
[providers."python:template"]
venv_name = "{{ python.venv }}"
shared = "from-main"
"#;

const DROP_IN_TEMPLATE: &str = r#"
[providers."python:template"]
version = "{{ python.version }}"
shared = "from-drop-in"
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_record_carries_the_label_of_the_file_that_set_it() {
    let e = Env::new(MAIN_TEMPLATE, &[("10-mine.toml", DROP_IN_TEMPLATE)]);
    let _h = e.serve().await;
    let mut s = e.connect().await;

    let main = e.label("config.toml");
    let drop_in = e.label("conf.d/10-mine.toml");
    let static_kind = json!({"kind": "static"});

    assert_eq!(
        row(&mut s, "python:template", "venv_name").await,
        Some((
            main.clone(),
            json!("{{ python.venv }}"),
            static_kind.clone()
        )),
        "a leaf only the main file sets keeps the main file's label"
    );
    assert_eq!(
        row(&mut s, "python:template", "version").await,
        Some((
            drop_in.clone(),
            json!("{{ python.version }}"),
            static_kind.clone()
        )),
        "a leaf only the drop-in sets carries the drop-in's label"
    );
    assert_eq!(
        row(&mut s, "python:template", "shared").await,
        Some((drop_in, json!("from-drop-in"), static_kind)),
        "a leaf both files set carries the later file's label and value"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_facet_read_returns_every_seeded_leaf() {
    let e = Env::new(MAIN_TEMPLATE, &[("10-mine.toml", DROP_IN_TEMPLATE)]);
    let _h = e.serve().await;
    let mut s = e.connect().await;

    assert_eq!(
        data(&mut s, "python:template").await,
        json!({
            "venv_name": "{{ python.venv }}",
            "version": "{{ python.version }}",
            "shared": "from-drop-in",
        }),
        "both layers merge into one facet tree"
    );
}

// ── (b) a namespace with no provider still answers with its template ─────────

const SCANLINE: &str = r#"
[providers."scanline:template"]
version = "{{ v }}"

[providers."scanline:template".widgets]
foo = "{{ bar }}"
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_seeded_template_answers_a_get_on_a_namespace_with_no_provider() {
    let e = Env::new("", &[("10-scanline.toml", SCANLINE)]);
    let _h = e.serve().await;
    let mut s = e.connect().await;

    assert_eq!(
        template(&mut s, "scanline.version").await,
        json!("{{ v }}"),
        "the template half comes from the seed"
    );
    assert_eq!(
        template(&mut s, "scanline.widgets.foo").await,
        json!("{{ bar }}"),
        "a nested table is a group in the template tree"
    );
    assert_eq!(
        template(&mut s, "scanline").await,
        json!({"version": "{{ v }}", "widgets": {"foo": "{{ bar }}"}}),
        "a whole-namespace read returns the whole template tree"
    );
}

// ── (c) validation ───────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dotted_key_stops_the_daemon_starting() {
    let e = Env::new("[providers.\"x:template\"]\n\"a.b\" = \"{{ y }}\"\n", &[]);
    let (_h, ready) = e.start();
    assert!(
        ready.await.is_err(),
        "a dotted key under a template facet is a config error"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_settings_key_in_a_facet_table_stops_the_daemon_starting() {
    let e = Env::new("[providers.\"x:template\"]\nenabled = false\n", &[]);
    let (_h, ready) = e.start();
    assert!(ready.await.is_err(), "a facet table holds data only");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reserved_facet_name_stops_the_daemon_starting() {
    let e = Env::new("[providers.\"x:age\"]\nn = \"1\"\n", &[]);
    let (_h, ready) = e.start();
    assert!(
        ready.await.is_err(),
        "`age` is a metadata suffix, not a facet"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_facet_key_stops_the_daemon_starting() {
    let e = Env::new("[providers.\"a:b:c\"]\nn = \"1\"\n", &[]);
    let (_h, ready) = e.start();
    assert!(ready.await.is_err(), "two colons is not facet syntax");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_non_string_leaf_is_dropped_from_the_seed() {
    let e = Env::new(
        "[providers.\"x:template\"]\nwidth = 12\nname = \"{{ n }}\"\n",
        &[],
    );
    let _h = e.serve().await;
    let mut s = e.connect().await;

    assert_eq!(
        data(&mut s, "x:template").await,
        json!({"name": "{{ n }}"}),
        "a leaf that is not a value expression is warned about and dropped"
    );
}

// ── (d) live re-seed ─────────────────────────────────────────────────────────

const RESEED_V1: &str = r#"
[providers."scanline:template"]
version = "v1"
title = "t1"
"#;

const RESEED_V2: &str = r#"
[providers."scanline:template"]
version = "v1"
title = "t2"
"#;

const RESEED_V3: &str = r#"
[providers."scanline:template"]
title = "t2"
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_facet_only_edit_re_seeds_in_place_and_leaves_put_records_alone() {
    let e = Env::new("", &[("10-mine.toml", RESEED_V1)]);
    let _h = e.serve().await;
    let mut s = e.connect().await;

    let resp = send_recv(
        &mut s,
        json!({"op": "put", "key": "scanline:template.note", "data": "kept"}),
    )
    .await;
    assert_eq!(resp["ok"], json!(true), "put: {resp}");

    // An edited leaf takes effect on the next read, with no restart.
    rewrite(&e.drop_in("10-mine.toml"), RESEED_V2);
    common::poll::poll_until_async(
        "scanline:template.title",
        "the edited value",
        Duration::from_secs(5),
        Duration::from_millis(20),
        async || data(&mut s, "scanline:template.title").await,
        |v| *v == json!("t2"),
    )
    .await;

    assert_eq!(
        data(&mut s, "scanline:template.version").await,
        json!("v1"),
        "a leaf the edit did not touch is re-seeded unchanged"
    );
    assert_eq!(
        data(&mut s, "scanline:template.note").await,
        json!("kept"),
        "a put record survives a re-seed"
    );

    // A leaf deleted from its file disappears; the put record still does not.
    rewrite(&e.drop_in("10-mine.toml"), RESEED_V3);
    common::poll::poll_until_async(
        "scanline:template.version",
        "a miss, once the leaf is gone from the file",
        Duration::from_secs(5),
        Duration::from_millis(20),
        async || data(&mut s, "scanline:template.version").await,
        |v| v.is_null(),
    )
    .await;

    assert_eq!(
        data(&mut s, "scanline:template.note").await,
        json!("kept"),
        "only `config:` records are dropped by a re-seed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_wakes_on_a_config_edit() {
    let e = Env::new("", &[("10-mine.toml", RESEED_V1)]);
    let _h = e.serve().await;

    let mut w = e.connect().await;
    let mut line = String::new();
    w.write_all(b"{\"op\":\"watch\",\"key\":\"scanline.title\"}\n")
        .await
        .unwrap();
    let mut reader = BufReader::new(&mut w);
    reader.read_line(&mut line).await.unwrap();
    let first: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(first["template"], json!("t1"), "initial frame: {first}");

    rewrite(&e.drop_in("10-mine.toml"), RESEED_V2);

    let frame = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mut next = String::new();
            reader.read_line(&mut next).await.unwrap();
            let v: serde_json::Value = serde_json::from_str(&next).unwrap();
            if v["template"] == json!("t2") {
                return v;
            }
        }
    })
    .await
    .expect("a config edit wakes the watch");
    assert_eq!(frame["template"], json!("t2"));
}

/// A settings key in a facet table, plus an edit that would otherwise apply.
/// The settings key is an error the cache could not catch on its own — nothing
/// about `enabled = false` is unwritable — so only the validation pass in front
/// of the re-seed stops the `title` edit beside it from landing.
const RESEED_INVALID: &str = r#"
[providers."scanline:template"]
version = "v1"
title = "t2"
enabled = false
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invalid_edit_keeps_the_previous_seed() {
    let e = Env::new("", &[("10-mine.toml", RESEED_V1)]);
    let _h = e.serve().await;
    let mut s = e.connect().await;

    // The re-seed applies nothing at all — not even the leaves of the same file
    // that are still fine.
    rewrite(&e.drop_in("10-mine.toml"), RESEED_INVALID);

    // The watch's poll interval plus its parse gate is well inside this window;
    // the point is that nothing changes once it has run. A throwaway connection
    // keeps the timeout from stranding a half-read response on the shared one.
    let settled = {
        let mut probe = e.connect().await;
        tokio::time::timeout(
            Duration::from_millis(600),
            common::poll::poll_until_async(
                "scanline:template.title",
                "any change at all",
                Duration::from_secs(30),
                Duration::from_millis(20),
                async || data(&mut probe, "scanline:template.title").await,
                |v| *v != json!("t1"),
            ),
        )
        .await
    };
    assert!(
        settled.is_err(),
        "an invalid edit must leave the previous seed in place, got {settled:?}"
    );
    assert_eq!(
        data(&mut s, "scanline:template.version").await,
        json!("v1"),
        "and every other leaf of the previous seed too"
    );

    // The daemon is still watching: the next valid edit is applied.
    rewrite(&e.drop_in("10-mine.toml"), RESEED_V2);
    common::poll::poll_until_async(
        "scanline:template.title",
        "the edited value, once the file is valid again",
        Duration::from_secs(5),
        Duration::from_millis(20),
        async || data(&mut s, "scanline:template.title").await,
        |v| *v == json!("t2"),
    )
    .await;
}

// ── the swap is one guard ────────────────────────────────────────────────────
//
// Driven against `Cache::reseed_static` directly rather than through the
// daemon: the window a split drop-then-write would open is nanoseconds wide,
// and one re-seed behind a socket round trip never lands in it — a test at that
// level passes whether or not the implementation is atomic, which is worse than
// no test at all.

fn template_tree(version: &str, title: &str) -> CacheValue {
    CacheValue::Object(HashMap::from([
        ("version".to_string(), CacheValue::String(version.into())),
        ("title".to_string(), CacheValue::String(title.into())),
    ]))
}

#[test]
fn a_re_seed_never_shows_a_gap_to_a_concurrent_read() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    registry.register_static("scanline:template");
    let label = "config:/x/10-mine.toml".to_string();

    cache
        .reseed_static(
            "scanline:template",
            None,
            "config:",
            &[(label.clone(), template_tree("v1", "t1"))],
            &registry,
        )
        .unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let (cache, registry, stop) = (cache.clone(), registry.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut reads = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let read = cache
                    .read_merged("scanline:template", None, &registry)
                    .expect("the namespace is never empty");
                assert!(
                    read.fields.contains_key("version"),
                    "a concurrent read saw the gap between the drop and the writes"
                );
                reads += 1;
            }
            reads
        })
    };

    // Thousands of swaps: a split drop-then-write leaves a window on every one
    // of them, and the reader is hammering the same entry.
    for i in 0..5000 {
        cache
            .reseed_static(
                "scanline:template",
                None,
                "config:",
                &[(label.clone(), template_tree("v1", &format!("t{i}")))],
                &registry,
            )
            .unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    let reads = reader.join().unwrap();
    assert!(reads > 0, "the reader thread never ran");
}

#[test]
fn a_refused_write_leaves_the_previous_seed_untouched() {
    // The drop and the writes are one operation, so a write the cache refuses
    // cannot leave the entry emptied behind it. `hostname.name` is produced by
    // the provider's `host` source, so the second write is refused — and the
    // first must not have landed, nor the previous seed have gone.
    let cache = Cache::new();
    let registry = ProviderRegistry::with_defaults();
    let old = "config:/x/old.toml".to_string();

    cache
        .reseed_static(
            "hostname",
            None,
            "config:",
            &[(
                old.clone(),
                CacheValue::Object(HashMap::from([(
                    "note".to_string(),
                    CacheValue::String("kept".into()),
                )])),
            )],
            &registry,
        )
        .unwrap();

    let err = cache
        .reseed_static(
            "hostname",
            None,
            "config:",
            &[
                (
                    "config:/x/new.toml".to_string(),
                    CacheValue::Object(HashMap::from([(
                        "other".to_string(),
                        CacheValue::String("new".into()),
                    )])),
                ),
                (
                    "config:/x/new.toml".to_string(),
                    CacheValue::Object(HashMap::from([(
                        "name".to_string(),
                        CacheValue::String("boom".into()),
                    )])),
                ),
            ],
            &registry,
        )
        .expect_err("a Source-owned record refuses the swap");
    assert!(format!("{err}").contains("hostname.name"), "{err}");

    let read = cache
        .read_merged("hostname", None, &registry)
        .expect("the previous seed is still there");
    assert_eq!(
        read.fields.get("note"),
        Some(&CacheValue::String("kept".into())),
        "the refusal happened before the drop"
    );
    assert!(
        !read.fields.contains_key("other"),
        "and before the first write of the batch too"
    );
}

// ── (e) a settings change still restarts ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_settings_change_shuts_the_daemon_down_for_restart() {
    let e = Env::new("[daemon]\nlog_level = \"debug\"\n", &[]);
    let handle = e.serve().await;

    rewrite(
        &e.cfg_dir.join("config.toml"),
        "[daemon]\nlog_level = \"trace\"\n",
    );

    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("a settings change shuts the daemon down")
        .expect("the daemon task did not panic");
}

// ── (f) `comb check config` reports the error ────────────────────────────────

#[test]
fn comb_check_config_reports_a_facet_error() {
    let tmp = TempDir::new().unwrap();
    let cfg_dir = tmp.path().join("beachcomber");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        "[providers.\"x:template\"]\n\"a.b\" = \"{{ y }}\"\n",
    )
    .unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_comb"))
        .env("XDG_CONFIG_HOME", tmp.path())
        .env("BEACHCOMBER_SOCKET", tmp.path().join("no-daemon.sock"))
        .args(["check", "--connect-timeout", "0", "config"])
        .output()
        .expect("run comb check config");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("a.b"),
        "`comb check config` must name the offending key, got:\n{stdout}"
    );
    assert_eq!(out.status.code(), Some(2), "a config error exits 2");
}
