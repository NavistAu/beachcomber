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
use tokio_util::sync::CancellationToken;

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
/// poll sees the change whatever the clock's granularity.
fn rewrite(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    common::mtime::touch_future(path);
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
    assert_eq!(resp["ok"], json!(true), "status: {resp}");
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

/// The errors `Config::load()` produces for the config `Env` just wrote — the
/// same list `run_daemon_with_cancel` refuses to start on, and the same list
/// `comb check config` prints.
fn startup_errors() -> Vec<String> {
    let registry = ProviderRegistry::with_defaults();
    Config::load()
        .validate_providers(
            &registry.provider_names(),
            &registry.source_names_by_provider(),
        )
        .1
}

/// Assert the message, then assert the wiring: the message is what an operator
/// reads, and `ready` erroring is the only proof the daemon acts on it.
async fn refuses_to_start(e: &Env, expected: &str) {
    assert_eq!(
        startup_errors(),
        vec![expected.to_string()],
        "validation must produce exactly this error"
    );
    let (_h, ready) = e.start();
    assert!(
        ready.await.is_err(),
        "and the daemon must refuse to start on it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dotted_key_stops_the_daemon_starting() {
    let e = Env::new("[providers.\"x:template\"]\n\"a.b\" = \"{{ y }}\"\n", &[]);
    refuses_to_start(
        &e,
        "[providers.\"x:template\"] key 'a.b' contains a '.': a facet table nests with tables, \
         and a dotted key names one record while addressing another.",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_settings_key_in_a_facet_table_stops_the_daemon_starting() {
    let e = Env::new("[providers.\"x:template\"]\nenabled = false\n", &[]);
    refuses_to_start(
        &e,
        "[providers.\"x:template\"] is a facet table and holds data only, but 'enabled' is a \
         settings key. It belongs in [providers.x].",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reserved_facet_name_stops_the_daemon_starting() {
    let e = Env::new("[providers.\"x:age\"]\nn = \"1\"\n", &[]);
    refuses_to_start(&e, "[providers.\"x:age\"]: invalid facet key: x:age").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_facet_key_stops_the_daemon_starting() {
    let e = Env::new("[providers.\"a:b:c\"]\nn = \"1\"\n", &[]);
    refuses_to_start(&e, "[providers.\"a:b:c\"]: invalid facet key: a:b:c").await;
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
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
        .await
        .expect("the watch sends an initial frame")
        .unwrap();
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

/// The re-seed's reaction to an invalid edit is that *nothing happens*, and a
/// daemon-level test can only chase that with a negative window — a wait long
/// enough to be convincing and short enough to be fast, which is neither.
///
/// So drive the watch directly, with the closure production uses, and wait on
/// the call itself: once `reseed_facets` has run and returned, "nothing
/// happened" is a settled fact rather than a guess about timing.
#[test]
fn an_invalid_edit_is_rejected_and_the_next_valid_one_applied() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "").unwrap();
    let conf_d = tmp.path().join("conf.d");
    std::fs::create_dir(&conf_d).unwrap();
    let drop_in = conf_d.join("10-mine.toml");
    std::fs::write(&drop_in, RESEED_V1).unwrap();

    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let layers = || Config::layers(Some(&cfg), &conf_d);
    beachcomber::daemon::seed::seed_facets(&cache, &registry, &layers());
    let leaf = |name: &str| {
        cache
            .read_merged("scanline:template", None, &registry)
            .and_then(|read| read.fields.get(name).cloned())
    };
    assert_eq!(leaf("title"), Some(CacheValue::String("t1".into())));

    let (tx, rx) = std::sync::mpsc::channel();
    let (watch_cache, watch_registry) = (cache.clone(), registry.clone());
    let (watch_cfg, watch_conf_d) = (cfg.clone(), conf_d.clone());
    let stop = CancellationToken::new();
    beachcomber::daemon::config_watch::spawn_config_self_watch_with(
        cfg.clone(),
        Duration::from_millis(20),
        false,
        stop.clone(),
        move |kind| {
            if kind == beachcomber::daemon::config_watch::ChangeKind::FacetsOnly {
                let layers = Config::layers(Some(&watch_cfg), &watch_conf_d);
                beachcomber::daemon::seed::reseed_facets(&watch_cache, &watch_registry, &layers);
            }
            let _ = tx.send(kind);
        },
    );

    std::fs::write(&drop_in, RESEED_INVALID).unwrap();
    common::mtime::touch_future(&drop_in);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3)),
        Ok(beachcomber::daemon::config_watch::ChangeKind::FacetsOnly),
        "the watch sees the edit"
    );
    assert_eq!(
        leaf("title"),
        Some(CacheValue::String("t1".into())),
        "an invalid edit applies nothing — not even the leaves of it that are fine"
    );
    assert_eq!(
        leaf("version"),
        Some(CacheValue::String("v1".into())),
        "and leaves the rest of the previous seed alone"
    );

    std::fs::write(&drop_in, RESEED_V2).unwrap();
    common::mtime::touch_future(&drop_in);
    rx.recv_timeout(Duration::from_secs(3))
        .expect("the watch is still armed after rejecting an edit");
    assert_eq!(
        leaf("title"),
        Some(CacheValue::String("t2".into())),
        "the next valid edit is applied"
    );
    stop.cancel();
}

/// A namespace dropped from configuration entirely: its records go, and the
/// entry with them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_namespace_removed_from_config_disappears() {
    let e = Env::new(
        "",
        &[(
            "10-mine.toml",
            "[providers.\"scanline:template\"]\nversion = \"v1\"\n\n\
             [providers.\"other:template\"]\nkeeper = \"k1\"\n",
        )],
    );
    let _h = e.serve().await;
    let mut s = e.connect().await;
    assert_eq!(data(&mut s, "scanline:template.version").await, json!("v1"));

    rewrite(
        &e.drop_in("10-mine.toml"),
        "[providers.\"other:template\"]\nkeeper = \"k2\"\n",
    );
    common::poll::poll_until_async(
        "other:template.keeper",
        "the edit that proves the re-seed ran",
        Duration::from_secs(5),
        Duration::from_millis(20),
        async || data(&mut s, "other:template.keeper").await,
        |v| *v == json!("k2"),
    )
    .await;

    assert!(
        data(&mut s, "scanline:template").await.is_null(),
        "the whole namespace is gone, not just its leaves"
    );
    assert_eq!(
        row(&mut s, "scanline:template", "version").await,
        None,
        "and its cache entry with it — no rows in status"
    );
}

/// Spec §3.2: a re-seed never touches a `put` record, including at a leaf
/// configuration also sets. Configuration seeds a default; a `put` is the
/// operator saying otherwise.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_put_record_wins_over_config_on_re_seed() {
    let e = Env::new("", &[("10-mine.toml", RESEED_V1)]);
    let _h = e.serve().await;
    let mut s = e.connect().await;

    let resp = send_recv(
        &mut s,
        json!({"op": "put", "key": "scanline:template.version", "data": "mine"}),
    )
    .await;
    assert_eq!(resp["ok"], json!(true), "put: {resp}");

    // RESEED_V2 sets `version` too, and edits `title` so there is something to
    // wait on that proves the re-seed ran.
    rewrite(&e.drop_in("10-mine.toml"), RESEED_V2);
    common::poll::poll_until_async(
        "scanline:template.title",
        "the edit that proves the re-seed ran",
        Duration::from_secs(5),
        Duration::from_millis(20),
        async || data(&mut s, "scanline:template.title").await,
        |v| *v == json!("t2"),
    )
    .await;

    assert_eq!(
        data(&mut s, "scanline:template.version").await,
        json!("mine"),
        "config must not overwrite the record the operator put there"
    );
    let (source, value, kind) = row(&mut s, "scanline:template", "version")
        .await
        .expect("the record is still in the cache");
    assert_eq!(
        source, "put",
        "and it is still the put record, not relabelled"
    );
    assert_eq!(value, json!("mine"));
    assert_eq!(kind, json!({"kind": "static"}));
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

    const SWAPS: u64 = 500;
    cache
        .reseed_static(
            "scanline:template",
            None,
            "config:",
            vec![(label.clone(), template_tree("v1", "t1"))],
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

    // A split drop-then-write leaves a window on every one of these, and the
    // reader is hammering the same entry.
    for i in 0..SWAPS {
        cache
            .reseed_static(
                "scanline:template",
                None,
                "config:",
                vec![(label.clone(), template_tree("v1", &format!("t{i}")))],
                &registry,
            )
            .unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    let reads = reader.join().unwrap();
    // A read is cheaper than a swap, so the reader should comfortably outpace
    // the writer. Anything less and the reader was not actually interleaved
    // with the swaps, and its silence proves nothing.
    assert!(
        reads >= SWAPS,
        "the reader managed {reads} reads across {SWAPS} swaps — too few to have interleaved"
    );
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
            vec![(
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
            vec![
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

/// The Config block carries two views — the daemon's and the client's own
/// reading of the file — as one verdict list under one count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn comb_check_config_renders_both_views_as_one_list() {
    // `width = 12` is a warning, not an error: the daemon starts, and the
    // client has exactly one thing to say that the daemon does not.
    let e = Env::new("[providers.\"x:template\"]\nwidth = 12\n", &[]);
    let _h = e.serve().await;

    let out = tokio::task::spawn_blocking({
        let (cfg_root, sock) = (e.cfg_dir.parent().unwrap().to_path_buf(), e.sock.clone());
        move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_comb"))
                .env("XDG_CONFIG_HOME", cfg_root)
                .env("BEACHCOMBER_SOCKET", sock)
                .args(["check", "--connect-timeout", "2s", "config"])
                .output()
                .expect("run comb check config")
        }
    })
    .await
    .unwrap();

    let expected = format!(
        "Config\n\
         \x20 [PASS] daemon   path {}\n\
         \x20 [PASS] daemon   parsed ok — 1 provider definitions\n\
         \x20 [PASS] daemon   config parsed\n\
         \x20 [PASS] daemon   1 provider definitions loaded\n\
         \x20 [WARN] local    [providers.\"x:template\"] 'width' is integer, not a value \
         expression or a group; it is dropped from the seed.\n\
         \n\
         (1 warning(s))\n\
         \n",
        e.cfg_dir.join("config.toml").display()
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        expected,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(1), "a warning exits 1");
}
