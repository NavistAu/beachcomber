//! Facet tables in configuration seed their namespaces at daemon start, one
//! `config:<file>` label per file, and re-seed in place when a file changes
//! (spec §3.1, §3.2, §3.9).
//!
//! Each test owns its `$HOME` and its `$XDG_CONFIG_HOME`: nextest runs every
//! test in its own process, so `set_var` here is process-local — the same
//! assumption `tests/suite/path_chain.rs` makes.

use crate::common::daemon::isolate;
use beachcomber::cache::{Cache, Layers};
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

pub(crate) struct Env {
    _home: TempDir,
    _cfg: TempDir,
    _sock: TempDir,
    pub(crate) cfg_dir: PathBuf,
    pub(crate) sock: PathBuf,
}

impl Env {
    /// Point `$HOME` and `$XDG_CONFIG_HOME` at fresh temp roots, then write
    /// `config.toml` and the named `conf.d` drop-ins.
    pub(crate) fn new(main: &str, drop_ins: &[(&str, &str)]) -> Env {
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

        // The daemon writes its managed defaults before it composes its
        // configuration; an in-process one is not `main`, so the fixture does.
        beachcomber::daemon::defaults::write_managed_defaults(
            &Config::defaults_dir_for(&cfg_dir.join("config.toml")),
            env!("BEACHCOMBER_VERSION"),
        )
        .unwrap();

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

    /// [`Self::start`] with a token the test holds, so it can tell a daemon
    /// that was told to shut down (as it does to restart) from one that was not.
    pub(crate) fn start_with(
        &self,
        cancel: CancellationToken,
    ) -> (
        tokio::task::JoinHandle<()>,
        beachcomber::daemon::DaemonReady,
    ) {
        beachcomber::daemon::start_in_process_polling_with(
            self.sock.clone(),
            Config::load(),
            cancel,
            None,
        )
    }

    /// Start and wait for the socket; panics if the daemon refuses to start.
    async fn serve(&self) -> tokio::task::JoinHandle<()> {
        let (handle, ready) = self.start();
        ready.await.expect("daemon ready");
        handle
    }

    pub(crate) async fn connect(&self) -> UnixStream {
        UnixStream::connect(&self.sock).await.unwrap()
    }

    /// How an error message names the file `rel`.
    fn label(&self, rel: &str) -> String {
        format!("config({})", self.cfg_dir.join(rel).display())
    }

    /// The label a `status` row presents for `rel`.
    fn row_label(&self, rel: &str) -> String {
        format!("config({})", self.cfg_dir.join(rel).display())
    }

    pub(crate) fn drop_in(&self, name: &str) -> PathBuf {
        self.cfg_dir.join("conf.d").join(name)
    }
}

/// Rewrite a config file and stamp its mtime into the future, so the watch's
/// poll sees the change whatever the clock's granularity.
pub(crate) fn rewrite(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    crate::common::mtime::touch_future(path);
}

pub(crate) async fn send_recv(
    stream: &mut UnixStream,
    request: serde_json::Value,
) -> serde_json::Value {
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
    row_from(stream, provider, field, None).await
}

/// `row`, restricted to the rows one source label holds: a field a `put` and a
/// config seed both set has a row under each, in no fixed order.
async fn row_from(
    stream: &mut UnixStream,
    provider: &str,
    field: &str,
    source: Option<&str>,
) -> Option<(String, serde_json::Value, serde_json::Value)> {
    let resp = send_recv(stream, json!({"op": "status"})).await;
    assert_eq!(resp["ok"], json!(true), "status: {resp}");
    resp["data"].as_array()?.iter().find_map(|r| {
        (r["provider"] == json!(provider)
            && r["field"] == json!(field)
            && source.is_none_or(|s| r["source"] == json!(s)))
        .then(|| {
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
[client.providers.python]
template.venv_name = "{{ python.venv }}"
template.shared = "from-main"
"#;

const DROP_IN_TEMPLATE: &str = r#"
[client.providers.python]
template.version = "{{ python.version }}"
template.shared = "from-drop-in"
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_templates_seed_the_template_facet_with_their_file_label() {
    let e = Env::new(MAIN_TEMPLATE, &[("10-mine.toml", DROP_IN_TEMPLATE)]);
    let _h = e.serve().await;
    let mut s = e.connect().await;

    let main = e.row_label("config.toml");
    let drop_in = e.row_label("conf.d/10-mine.toml");
    let static_kind = json!({"kind": "static"});

    // The shipped `defaults/python.toml` sets `venv_name` and `version` too:
    // each file's record is a row of its own, so each is looked up by label.
    assert_eq!(
        row_from(&mut s, "python:template", "venv_name", Some(&main)).await,
        Some((
            main.clone(),
            json!("{{ python.venv }}"),
            static_kind.clone()
        )),
        "a leaf the main file sets carries the main file's label"
    );
    assert_eq!(
        row_from(&mut s, "python:template", "version", Some(&drop_in)).await,
        Some((
            drop_in.clone(),
            json!("{{ python.version }}"),
            static_kind.clone()
        )),
        "a leaf the drop-in sets carries the drop-in's label"
    );
    assert_eq!(
        row_from(&mut s, "python:template", "shared", Some(&drop_in)).await,
        Some((drop_in, json!("from-drop-in"), static_kind.clone())),
        "a leaf both files set is a record of each, the later file's"
    );
    assert_eq!(
        row_from(&mut s, "python:template", "shared", Some(&main)).await,
        Some((main, json!("from-main"), static_kind)),
        "and the earlier file's, which it overrides"
    );
}

/// `aws.profile` set by the shipped `defaults/aws.toml`, by `config.toml` and
/// by a drop-in: a read takes the drop-in's, the `layers` behind it list all
/// three files, nearest first, and removing the drop-in shows `config.toml`'s
/// without a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_the_file_that_answers_shows_the_one_it_overrode() {
    let e = Env::new(
        "[client.providers.aws]\ntemplate.profile = \"from-main\"\n",
        &[(
            "10-mine.toml",
            "[client.providers.aws]\ntemplate.profile = \"from-drop-in\"\n",
        )],
    );
    let _h = e.serve().await;
    let mut s = e.connect().await;
    assert_eq!(
        data(&mut s, "aws:template.profile").await,
        json!("from-drop-in")
    );

    let resp = send_recv(&mut s, json!({"op": "layers", "key": "aws.profile"})).await;
    let layers = resp["data"]
        .as_array()
        .unwrap_or_else(|| panic!("layers: {resp}"));
    let labels: Vec<&str> = layers
        .iter()
        .map(|l| l["label"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        labels,
        vec![
            e.row_label("conf.d/10-mine.toml").as_str(),
            e.row_label("config.toml").as_str(),
            e.row_label("defaults/aws.toml").as_str(),
        ],
        "{resp}"
    );
    assert_eq!(layers[0]["answered"], json!(true), "{resp}");
    assert!(
        layers[1..].iter().all(|l| l.get("answered").is_none()),
        "the others are shadowed: {resp}"
    );

    // The watch snapshots `conf.d/`, so a file gone from it is a change.
    std::fs::remove_file(e.drop_in("10-mine.toml")).unwrap();
    crate::common::poll::poll_until_async(
        "aws:template.profile",
        "the record the drop-in overrode",
        Duration::from_secs(5),
        Duration::from_millis(5),
        async || data(&mut s, "aws:template.profile").await,
        |v| *v == json!("from-main"),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_later_file_replaces_one_template_leaf_and_keeps_the_rest() {
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

// ── (b) a client-only provider answers with its template ──────────────────────

const SCANLINE: &str = r#"
[client.providers.scanline]
template.version = "{{ v }}"

[client.providers.scanline.template.widgets]
foo = "{{ bar }}"
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_seeded_template_answers_a_get_on_a_client_only_provider() {
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
    let config = Config::load();
    ProviderRegistry::load(&config, &Default::default()).errors
}

/// Assert the message, then assert the wiring: the message is what an operator
/// reads, and `ready` erroring is the only proof the daemon acts on it. A rule
/// is checked per file, so the message is prefixed with the label of the file
/// (`rel`, relative to the config directory) that broke it.
async fn refuses_to_start(e: &Env, rel: &str, expected: &str) {
    assert_eq!(
        startup_errors(),
        vec![format!("{}: {expected}", e.label(rel))],
        "validation must produce exactly this error"
    );
    let (_h, ready) = e.start();
    assert!(
        ready.await.is_err(),
        "and the daemon must refuse to start on it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_key_in_a_client_provider_stops_the_daemon_starting() {
    let e = Env::new("[client.providers.x]\nenabled = false\n", &[]);
    refuses_to_start(
        &e,
        "config.toml",
        "unknown key 'enabled' in [client.providers.x]",
    )
    .await;
}

/// `path` is not a provider setting any more (the path-expression setting is
/// gone), so a template may hold a leaf by that name: the table validates clean
/// and the leaf is seeded like any other.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_leaf_named_path_in_a_template_is_data() {
    let e = Env::new("[client.providers.git]\ntemplate.path = \"x\"\n", &[]);
    assert_eq!(startup_errors(), Vec::<String>::new());
    let _h = e.serve().await;
    let mut s = e.connect().await;

    assert_eq!(data(&mut s, "git:template.path").await, json!("x"));
}

/// A table of the wrong type is an error the validation reports, not a
/// silently emptied config.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_providers_key_that_is_not_a_table_stops_the_daemon_starting() {
    let e = Env::new("[client]\nproviders = 3\n", &[]);
    refuses_to_start(&e, "config.toml", "[client.providers] must be a table").await;
    let e = Env::new("[daemon]\nproviders = 3\n", &[]);
    refuses_to_start(&e, "config.toml", "[daemon.providers] must be a table").await;
}

/// The `[providers]` root, colon-named facet tables included, is removed, not
/// deprecated: it is an unknown table that says where its contents went.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_old_facet_table_is_a_config_error() {
    for name in ["x:template", "*:cache"] {
        let e = Env::new(&format!("[providers.\"{name}\"]\nn = \"1\"\n"), &[]);
        refuses_to_start(
            &e,
            "config.toml",
            "unknown table [providers]: a provider's settings are [daemon.providers.<p>] (the \
             daemon's) and [client.providers.<p>] (the client's); a built-in Source is retuned \
             by [daemon.providers.<p>.sources.<s>] return; templates live in \
             [client.providers.<p>] template.<leaf>, decay in [daemon.providers.<p>.cache._]",
        )
        .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_non_string_leaf_stops_the_daemon_starting() {
    let e = Env::new(
        "[client.providers.x]\ntemplate.width = 12\ntemplate.name = \"{{ n }}\"\n",
        &[],
    );
    refuses_to_start(
        &e,
        "config.toml",
        "[client.providers.x] template 'width' is integer, not a value expression or a group.",
    )
    .await;
}

/// A rule is checked in the file that holds the key, because seeding is per
/// file: a leaf that is an integer in `config.toml` is refused there even
/// though a drop-in's string would win the merge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_leaf_is_refused_in_its_own_file_not_in_the_merge() {
    let e = Env::new(
        "[client.providers.zz]\ntemplate.width = 12\n",
        &[(
            "10-fix.toml",
            "[client.providers.zz]\ntemplate.width = \"{{ 1 }}\"\n",
        )],
    );
    refuses_to_start(
        &e,
        "config.toml",
        "[client.providers.zz] template 'width' is integer, not a value expression or a group.",
    )
    .await;
}

/// `Config::load` falls back to the default configuration when the composed
/// file does not deserialize; `client` must not be able to reach that, nor let
/// the seeding write what was never checked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_is_not_a_table_stops_the_daemon_starting() {
    let e = Env::new(
        "client = \"oops\"\n\n[daemon.providers.git.cache._]\ndecay = 0\n",
        &[],
    );
    assert_eq!(
        startup_errors(),
        vec![
            format!("{}: [client] must be a table", e.label("config.toml")),
            format!(
                "{}: git:cache._.decay must be an integer from 1 to 16 (got 0)",
                e.label("config.toml")
            ),
        ]
    );
    let (_h, ready) = e.start();
    assert!(ready.await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn names_that_spell_a_prototype_or_nothing_are_not_provider_names() {
    for (root, name, msg) in [
        (
            "client",
            "*",
            "invalid provider name '*' in [client.providers]: '*' is reserved: \
             *:cache._.decay is the decay of every provider",
        ),
        (
            "daemon",
            "*",
            "invalid provider name '*' in [daemon.providers]: '*' is reserved: \
             *:cache._.decay is the decay of every provider",
        ),
        (
            "client",
            "_",
            "invalid provider name '_' in [client.providers]: '_' is reserved: \
             it names a prototype table",
        ),
        (
            "client",
            "env",
            "invalid provider name 'env' in [client.providers]: 'env' is reserved: \
             env.X is the caller's environment",
        ),
        (
            "client",
            "",
            "invalid provider name '' in [client.providers]: a name is non-empty, holds no ':', \
             '.', '[', ']' or '*', and does not start with '/', '~' or '<'",
        ),
        (
            "daemon",
            "",
            "invalid provider name '' in [daemon.providers]: a name is non-empty, holds no ':', \
             '.', '[', ']' or '*', and does not start with '/', '~' or '<'",
        ),
    ] {
        let e = Env::new(&format!("[{root}.providers.\"{name}\"]\n"), &[]);
        refuses_to_start(&e, "config.toml", msg).await;
    }
}

/// A provider name is the head of a key, so it holds nothing the key grammar
/// reads as an address and never looks like a directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_name_the_key_grammar_cannot_head_is_not_a_provider_name() {
    for name in ["a[b", "a]b", "/abs", "~home", "<x>", "a*b"] {
        let e = Env::new(&format!("[client.providers.\"{name}\"]\n"), &[]);
        refuses_to_start(
            &e,
            "config.toml",
            &format!(
                "invalid provider name '{name}' in [client.providers]: a name is non-empty, holds \
                 no ':', '.', '[', ']' or '*', and does not start with '/', '~' or '<'"
            ),
        )
        .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_template_leaf_name_stops_the_daemon_starting() {
    let e = Env::new("[client.providers.x]\ntemplate.\"\" = \"e\"\n", &[]);
    refuses_to_start(
        &e,
        "config.toml",
        "[client.providers.x] template has an empty key",
    )
    .await;
}

/// Decay is capped at 16, a conservative bound: 2^16 times the base lifespan is
/// already far beyond any useful decay, and 32 would overflow the shift.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decay_above_sixteen_is_refused_in_config_and_by_put() {
    let e = Env::new("[daemon.providers.git.cache._]\ndecay = 17\n", &[]);
    refuses_to_start(
        &e,
        "config.toml",
        "git:cache._.decay must be an integer from 1 to 16 (got 17)",
    )
    .await;

    let e = Env::new("", &[]);
    let _h = e.serve().await;
    let mut s = e.connect().await;
    for (bad, shown) in [
        (json!(17), "17"),
        (json!(0), "0"),
        (json!("many"), "\"many\""),
    ] {
        let resp = send_recv(
            &mut s,
            json!({"op": "put", "key": "git:cache._.decay", "data": bad}),
        )
        .await;
        assert_eq!(resp["ok"], json!(false), "{resp}");
        assert_eq!(
            resp["error"],
            json!(format!(
                "git:cache._.decay must be an integer from 1 to 16 (got {shown})"
            ))
        );
    }
    let resp = send_recv(
        &mut s,
        json!({"op": "put", "key": "git:cache._.decay", "data": 16}),
    )
    .await;
    assert_eq!(resp["ok"], json!(true), "16 is the cap and allowed: {resp}");
}

// ── a client-only provider is a provider ───────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_only_provider_is_a_provider() {
    let e = Env::new(
        "[client.providers.aws]\ntemplate.profile = \"{{ env.P }}\"\n",
        &[],
    );
    let _h = e.serve().await;
    let mut s = e.connect().await;

    let resp = send_recv(&mut s, json!({"op": "introspect", "subject": "providers"})).await;
    assert_eq!(resp["ok"], json!(true), "introspect providers: {resp}");
    let aws = resp["data"]["providers"]
        .as_array()
        .expect("providers array")
        .iter()
        .find(|p| p["name"] == json!("aws"))
        .unwrap_or_else(|| panic!("aws is not listed: {resp}"));
    assert_eq!(aws["sources"], json!([]), "a provider with zero Sources");
    assert_eq!(aws["source"], json!("static"), "and no code behind it");
    assert_eq!(
        template(&mut s, "aws.profile").await,
        json!("{{ env.P }}"),
        "and its template is served"
    );
}

#[test]
fn the_abstract_provider_is_never_registered() {
    let config = Config::from_toml(
        "[daemon.providers._.cache._]\ndecay = 4\n\n[client.providers.aws]\ntemplate.p = \"x\"\n",
    );
    let registry = ProviderRegistry::with_config(&config, &Default::default()).unwrap();
    assert!(registry.is_registered("aws"), "named in [client.providers]");
    assert!(
        !registry.is_registered("_"),
        "`_` is a prototype, not a provider"
    );
}

// ── the decay prototype is data seeded under `*:cache`  ─────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_decay_prototype_seeds_star_cache() {
    let e = Env::new("", &[]);
    beachcomber::daemon::defaults::write_managed_defaults(&e.cfg_dir.join("defaults"), "9.9.9")
        .unwrap();
    let _h = e.serve().await;
    let mut s = e.connect().await;

    assert_eq!(data(&mut s, "*:cache._.decay").await, json!(4));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_provider_decay_table_seeds_its_own_cache_facet() {
    let e = Env::new("[daemon.providers.git.cache._]\ndecay = 2\n", &[]);
    let _h = e.serve().await;
    let mut s = e.connect().await;
    assert_eq!(data(&mut s, "git:cache._.decay").await, json!(2));

    // And the lookup the scheduler makes finds it.
    let cache = Cache::new();
    let registry = crate::common::registry::with_shipped_defaults();
    let main = e.cfg_dir.join("config.toml");
    beachcomber::daemon::seed::seed_facets(
        &cache,
        &registry,
        &Config::read_layers(
            Some(&e.cfg_dir.join("defaults")),
            Some(&main),
            &e.cfg_dir.join("conf.d"),
        )
        .layers,
    );
    assert_eq!(cache.resolve_decay("git", None), 2);
    assert_eq!(
        cache.resolve_decay("hostname", None),
        beachcomber::scheduler::lifecycle::DEFAULT_DECAY,
        "a provider with no table of its own takes the default"
    );
}

/// A `put` of the decay prototype shadows the seed, and removing the put shows
/// the seed again, as for every other seeded record.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_put_decay_shadows_the_seeded_decay_and_removing_it_restores_the_seed() {
    let e = Env::new("[daemon.providers.git.cache._]\ndecay = 2\n", &[]);
    let _h = e.serve().await;
    let mut s = e.connect().await;
    assert_eq!(data(&mut s, "git:cache._.decay").await, json!(2));

    let resp = send_recv(
        &mut s,
        json!({"op": "put", "key": "git:cache._.decay", "data": 5}),
    )
    .await;
    assert_eq!(resp["ok"], json!(true), "put: {resp}");
    assert_eq!(data(&mut s, "git:cache._.decay").await, json!(5));

    let resp = send_recv(
        &mut s,
        json!({"op": "put", "key": "git:cache._.decay", "data": null}),
    )
    .await;
    assert_eq!(resp["ok"], json!(true), "remove: {resp}");
    assert_eq!(
        data(&mut s, "git:cache._.decay").await,
        json!(2),
        "the seed shows again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decay_table_removed_on_reload_drops_the_seed() {
    let e = Env::new(
        "",
        &[
            (
                "10-mine.toml",
                "[daemon.providers.git.cache._]\ndecay = 2\n",
            ),
            (
                "20-other.toml",
                "[daemon.providers.other.cache._]\ndecay = 3\n",
            ),
        ],
    );
    let _h = e.serve().await;
    let mut s = e.connect().await;
    assert_eq!(data(&mut s, "git:cache._.decay").await, json!(2));

    rewrite(&e.drop_in("10-mine.toml"), "");
    crate::common::poll::poll_until_async(
        "git:cache._.decay",
        "a miss, once the table is gone from the file",
        Duration::from_secs(5),
        Duration::from_millis(5),
        async || data(&mut s, "git:cache._.decay").await,
        |v| v.is_null(),
    )
    .await;
    assert_eq!(data(&mut s, "other:cache._.decay").await, json!(3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decay_edit_re_seeds_in_place() {
    let e = Env::new(
        "",
        &[(
            "10-mine.toml",
            "[daemon.providers.git.cache._]\ndecay = 2\n",
        )],
    );
    let _h = e.serve().await;
    let mut s = e.connect().await;
    assert_eq!(data(&mut s, "git:cache._.decay").await, json!(2));

    rewrite(
        &e.drop_in("10-mine.toml"),
        "[daemon.providers.git.cache._]\ndecay = 3\n",
    );
    crate::common::poll::poll_until_async(
        "git:cache._.decay",
        "the edited decay",
        Duration::from_secs(5),
        Duration::from_millis(5),
        async || data(&mut s, "git:cache._.decay").await,
        |v| *v == json!(3),
    )
    .await;
}

// ── (d) live re-seed ─────────────────────────────────────────────────────────

const RESEED_V1: &str = r#"
[client.providers.scanline]
template.version = "v1"
template.title = "t1"
"#;

const RESEED_V2: &str = r#"
[client.providers.scanline]
template.version = "v1"
template.title = "t2"
"#;

const RESEED_V3: &str = r#"
[client.providers.scanline]
template.title = "t2"
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
    crate::common::poll::poll_until_async(
        "scanline:template.title",
        "the edited value",
        Duration::from_secs(5),
        Duration::from_millis(5),
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
    crate::common::poll::poll_until_async(
        "scanline:template.version",
        "a miss, once the leaf is gone from the file",
        Duration::from_secs(5),
        Duration::from_millis(5),
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

/// A reload reseeds every namespace, but a file whose records in a namespace
/// come back unchanged keeps their age: AGE says when that configuration
/// last changed, not when the daemon last reloaded. The records' stamps are
/// compared across a reload called directly, so no age has to grow first.
#[test]
fn a_reload_keeps_the_age_of_records_it_did_not_change() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[client.providers.other]\ntemplate.x = \"kept\"\n").unwrap();
    let conf_d = tmp.path().join("conf.d");
    std::fs::create_dir(&conf_d).unwrap();
    let drop_in = conf_d.join("10-mine.toml");
    std::fs::write(&drop_in, RESEED_V1).unwrap();
    // The managed defaults carry the line that writes `[client.providers]`
    // templates into `<p>:template`.
    let defaults = tmp.path().join("defaults");
    beachcomber::daemon::defaults::write_managed_defaults(&defaults, env!("CARGO_PKG_VERSION"))
        .unwrap();
    let layers = || Config::read_layers(Some(&defaults), Some(&cfg), &conf_d).layers;

    let cache = Arc::new(Cache::new());
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());
    beachcomber::daemon::seed::seed_facets(&cache, &registry, &layers());
    // When `namespace`'s records from `file` were last written.
    let stamp = |namespace: &str, file: &str| {
        let entry = cache.get_entry(namespace, None).expect(namespace);
        let found = entry
            .sources
            .iter()
            .find(|(label, _)| label.ends_with(file))
            .map(|(_, records)| records.last_refreshed);
        found.unwrap_or_else(|| {
            panic!(
                "no {file} records in {namespace}: {:?}",
                entry.sources.keys()
            )
        })
    };
    let untouched = stamp("other:template", "config.toml");
    let edited = stamp("scanline:template", "10-mine.toml");

    let handle = beachcomber::provider::registry::RegistryHandle::new(registry.clone());
    let (sink_tx, _sink_rx) = tokio::sync::mpsc::channel(16);
    let sink = beachcomber::scheduler::SchedulerHandle::new(sink_tx);
    let mut last = registry.clone();
    std::fs::write(&drop_in, RESEED_V2).unwrap();
    reload(&cache, &handle, &sink, &mut last, layers());

    let title = cache
        .read_merged("scanline:template", None, &[], &registry, Layers::All)
        .and_then(|read| read.fields.get("title").cloned());
    assert_eq!(
        title,
        Some(CacheValue::String("t2".into())),
        "the edit applied"
    );
    assert_eq!(
        stamp("other:template", "config.toml"),
        untouched,
        "another file's unchanged record keeps its age"
    );
    assert!(
        stamp("scanline:template", "10-mine.toml") > edited,
        "the edited file's records restamp"
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

/// An unknown key beside the template, plus an edit that would otherwise
/// apply. The unknown key is an error the cache could not catch on its own —
/// nothing about `enabled = false` is unwritable — so only the validation pass
/// in front of the re-seed stops the `title` edit beside it from landing.
const RESEED_INVALID: &str = r#"
[client.providers.scanline]
enabled = false
template.version = "v1"
template.title = "t2"
"#;

/// Reload `layers` as the watch does: the registry its gate would have loaded
/// from them, adopted and sent to a scheduler that is never run. The facets are
/// reseeded before the message is sent, so the call is settled once it returns.
fn reload(
    cache: &Arc<Cache>,
    handle: &beachcomber::provider::registry::RegistryHandle,
    sink: &beachcomber::scheduler::SchedulerHandle,
    last: &mut Arc<ProviderRegistry>,
    layers: Vec<(PathBuf, toml::Value)>,
) {
    let changed = Config::from_layers(layers);
    let loaded = ProviderRegistry::load(&changed, &HashMap::new());
    beachcomber::daemon::seed::reload_providers(
        cache,
        handle,
        sink,
        last,
        &changed,
        loaded.registry,
    );
}

/// An invalid edit changes nothing: a reload refuses the layers when called
/// directly, which is settled once the call returns. (The watch's gate
/// reports no such edit at all; `config_watch.rs` tests that.)
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
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());
    let defaults = tmp.path().join("defaults");
    beachcomber::daemon::defaults::write_managed_defaults(&defaults, env!("CARGO_PKG_VERSION"))
        .unwrap();
    let layers = || Config::read_layers(Some(&defaults), Some(&cfg), &conf_d).layers;
    beachcomber::daemon::seed::seed_facets(&cache, &registry, &layers());
    let leaf = |name: &str| {
        cache
            .read_merged("scanline:template", None, &[], &registry, Layers::All)
            .and_then(|read| read.fields.get(name).cloned())
    };
    assert_eq!(leaf("title"), Some(CacheValue::String("t1".into())));

    let (tx, rx) = std::sync::mpsc::channel();
    let handle = beachcomber::provider::registry::RegistryHandle::new(registry.clone());
    let (sink_tx, _sink_rx) = tokio::sync::mpsc::channel(16);
    let sink = beachcomber::scheduler::SchedulerHandle::new(sink_tx);
    let mut last = registry.clone();
    let (watch_cache, watch_handle, watch_sink) = (cache.clone(), handle.clone(), sink.clone());
    let (watch_cfg, watch_conf_d, watch_defaults) = (cfg.clone(), conf_d.clone(), defaults.clone());
    let stop = CancellationToken::new();
    beachcomber::daemon::config_watch::spawn_config_self_watch_with(
        &Config::load_from(&cfg),
        Duration::from_millis(10),
        false,
        stop.clone(),
        move |kind, _, _| {
            if kind == beachcomber::daemon::config_watch::ChangeKind::Providers {
                let layers =
                    Config::read_layers(Some(&watch_defaults), Some(&watch_cfg), &watch_conf_d)
                        .layers;
                let mut last = watch_handle.current();
                reload(&watch_cache, &watch_handle, &watch_sink, &mut last, layers);
            }
            let _ = tx.send(kind);
        },
    );

    std::fs::write(&drop_in, RESEED_INVALID).unwrap();
    crate::common::mtime::touch_future(&drop_in);
    reload(&cache, &handle, &sink, &mut last, layers());
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
    crate::common::mtime::touch_future(&drop_in);
    rx.recv_timeout(Duration::from_secs(3))
        .expect("a valid edit after the invalid one is reported");
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
            "[client.providers.scanline]\ntemplate.version = \"v1\"\n\n\
             [client.providers.other]\ntemplate.keeper = \"k1\"\n",
        )],
    );
    let _h = e.serve().await;
    let mut s = e.connect().await;
    assert_eq!(data(&mut s, "scanline:template.version").await, json!("v1"));

    rewrite(
        &e.drop_in("10-mine.toml"),
        "[client.providers.other]\ntemplate.keeper = \"k2\"\n",
    );
    crate::common::poll::poll_until_async(
        "other:template.keeper",
        "the edit that proves the re-seed ran",
        Duration::from_secs(5),
        Duration::from_millis(5),
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
    crate::common::poll::poll_until_async(
        "scanline:template.title",
        "the edit that proves the re-seed ran",
        Duration::from_secs(5),
        Duration::from_millis(5),
        async || data(&mut s, "scanline:template.title").await,
        |v| *v == json!("t2"),
    )
    .await;

    assert_eq!(
        data(&mut s, "scanline:template.version").await,
        json!("mine"),
        "config must not overwrite the record the operator put there"
    );
    let (source, value, kind) = row_from(&mut s, "scanline:template", "version", Some("put"))
        .await
        .expect("the record is still in the cache");
    assert_eq!(
        source, "put",
        "and it is still the put record, not relabelled"
    );
    assert_eq!(value, json!("mine"));
    assert_eq!(kind, json!({"kind": "static"}));
}

/// Removing a put template shows the seeded one at once, with no re-seed.
#[test]
fn removing_a_put_template_shows_the_seeded_one() {
    let cache = Cache::new();
    let registry = crate::common::registry::with_shipped_defaults();
    registry.register_static("p:template");
    cache
        .reseed_static(
            "p:template",
            None,
            "config:",
            vec![(
                "config:/x/10.toml".to_string(),
                template_tree("seeded", "t"),
            )],
        )
        .unwrap();
    cache
        .write_static(
            "p:template",
            None,
            "put",
            "version",
            CacheValue::String("mine".into()),
            None,
        )
        .unwrap();
    let version = || {
        cache
            .read_merged("p:template", None, &[], &registry, Layers::All)
            .and_then(|r| r.fields.get("version").cloned())
    };
    assert_eq!(version(), Some(CacheValue::String("mine".into())));
    cache.remove_put("p:template", None, "version");
    assert_eq!(version(), Some(CacheValue::String("seeded".into())));
}

/// A re-seed leaves a put template on top of the new seed.
#[test]
fn a_reseed_keeps_a_put_template_on_top() {
    let cache = Cache::new();
    let registry = crate::common::registry::with_shipped_defaults();
    registry.register_static("p:template");
    cache
        .write_static(
            "p:template",
            None,
            "put",
            "version",
            CacheValue::String("mine".into()),
            None,
        )
        .unwrap();
    cache
        .reseed_static(
            "p:template",
            None,
            "config:",
            vec![(
                "config:/x/10.toml".to_string(),
                template_tree("seeded", "t"),
            )],
        )
        .unwrap();
    let read = cache
        .read_merged("p:template", None, &[], &registry, Layers::All)
        .unwrap();
    assert_eq!(
        read.fields.get("version"),
        Some(&CacheValue::String("mine".into()))
    );
    assert_eq!(
        read.fields.get("title"),
        Some(&CacheValue::String("t".into()))
    );
    // The seed is stored beneath the put, not skipped.
    cache.remove_put("p:template", None, "version");
    let read = cache
        .read_merged("p:template", None, &[], &registry, Layers::All)
        .unwrap();
    assert_eq!(
        read.fields.get("version"),
        Some(&CacheValue::String("seeded".into()))
    );
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
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());
    registry.register_static("scanline:template");
    let label = "config:/x/10-mine.toml".to_string();

    const SWAPS: u64 = 500;
    cache
        .reseed_static(
            "scanline:template",
            None,
            "config:",
            vec![(label.clone(), template_tree("v1", "t1"))],
        )
        .unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let reader = {
        let (cache, registry, stop) = (cache.clone(), registry.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut reads = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let read = cache
                    .read_merged("scanline:template", None, &[], &registry, Layers::All)
                    .expect("the namespace is never empty");
                assert!(
                    read.fields.contains_key("version"),
                    "a concurrent read saw the gap between the drop and the writes"
                );
                reads += 1;
                if reads == 1 {
                    let _ = started_tx.send(());
                }
            }
            reads
        })
    };
    // The overlap is structural, not a guess: the reader has read once before
    // the first swap and does not stop until told to, so every swap below
    // happens with it running.
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the reader thread started");

    // A split drop-then-write leaves a window on every one of these, and the
    // reader is hammering the same entry.
    for i in 0..SWAPS {
        cache
            .reseed_static(
                "scanline:template",
                None,
                "config:",
                vec![(label.clone(), template_tree("v1", &format!("t{i}")))],
            )
            .unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    let reads = reader.join().unwrap();
    assert!(
        reads >= 2,
        "the reader read {reads} times — it must outlive the swaps, not just precede them"
    );
}

/// Every file that sets a leaf keeps its own record of it, under its own
/// label. A read takes the last file in composition order — `defaults/`, then
/// `config.toml`, then `conf.d/` — and the layers list every file's record,
/// the one that answers first. The labels sort in a different order on
/// purpose: composition order is the order the files were written in, not
/// their names'.
#[test]
fn every_file_keeps_its_record_and_the_last_file_answers() {
    let cache = Cache::new();
    let registry = crate::common::registry::with_shipped_defaults();
    let defaults = "config:/c/defaults/aws.toml";
    let main = "config:/c/config.toml";
    let drop_in = "config:/c/conf.d/10-mine.toml";
    let tree = |v: &str| {
        CacheValue::Object(HashMap::from([(
            "profile".to_string(),
            CacheValue::String(v.into()),
        )]))
    };
    cache
        .reseed_static(
            "aws:template",
            None,
            "config:",
            vec![
                (defaults.into(), tree("from-defaults")),
                (main.into(), tree("from-main")),
                (drop_in.into(), tree("from-drop-in")),
            ],
        )
        .unwrap();

    let read = cache
        .read_merged("aws:template", None, &[], &registry, Layers::All)
        .unwrap();
    assert_eq!(
        read.fields["profile"],
        CacheValue::String("from-drop-in".into())
    );
    assert_eq!(&*read.records["profile"].source, drop_in);

    let labels: Vec<String> = cache
        .layer_stacks("aws", None, &[])
        .into_iter()
        .filter(|(tail, _)| tail == "profile")
        .map(|(_, record)| record.label)
        .collect();
    assert_eq!(
        labels,
        vec![
            "config(/c/conf.d/10-mine.toml)",
            "config(/c/config.toml)",
            "config(/c/defaults/aws.toml)",
        ],
        "every file's record, the answering one first"
    );

    // The decay prototype is seeded the same way, and resolved the same way.
    let decay = |n: i64| {
        CacheValue::Object(HashMap::from([(
            "_".to_string(),
            CacheValue::Object(HashMap::from([("decay".to_string(), CacheValue::Int(n))])),
        )]))
    };
    cache
        .reseed_static(
            "*:cache",
            None,
            "config:",
            vec![(defaults.into(), decay(4)), (main.into(), decay(6))],
        )
        .unwrap();
    assert_eq!(cache.resolve_decay("git", None), 6);
}

#[test]
fn a_refused_write_leaves_the_previous_seed_untouched() {
    // The drop and the writes are one operation, so a write the cache refuses
    // cannot leave the entry emptied behind it. A root that is not an object
    // is refused, so the second write is refused — and the first must not
    // have landed, nor the previous seed have gone.
    let cache = Cache::new();
    let registry = crate::common::registry::with_shipped_defaults();
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
                    CacheValue::String("boom".into()),
                ),
            ],
        )
        .expect_err("a root that is not an object refuses the swap");
    assert!(format!("{err}").contains("must be an object"), "{err}");

    let read = cache
        .read_merged("hostname", None, &[], &registry, Layers::All)
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
        "[client.providers.x]\ntemplate.\"\" = \"{{ y }}\"\n",
    )
    .unwrap();

    let out = isolate(
        &mut std::process::Command::new(env!("CARGO_BIN_EXE_comb")),
        tmp.path(),
        tmp.path(),
        &tmp.path().join("no-daemon.sock"),
    )
    .args(["check", "--connect-timeout", "0", "config"])
    .output()
    .expect("run comb check config");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[client.providers.x] template has an empty key"),
        "`comb check config` must name the offending key, got:\n{stdout}"
    );
    assert_eq!(out.status.code(), Some(2), "a config error exits 2");
}

/// The Config block carries two views — the daemon's and the client's own
/// reading of the file — as one verdict list under one count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn comb_check_config_renders_both_views_as_one_list() {
    // A valid file: the daemon starts, and the client reads the same file for
    // itself. The fixture wrote the managed defaults at this binary's version,
    // as `comb daemon` does, so the defaults line is a pass.
    let e = Env::new("[client.providers.x]\ntemplate.a = \"{{ x.b }}\"\n", &[]);
    let _h = e.serve().await;

    let out = tokio::task::spawn_blocking({
        let (cfg_root, sock) = (e.cfg_dir.parent().unwrap().to_path_buf(), e.sock.clone());
        move || {
            isolate(
                &mut std::process::Command::new(env!("CARGO_BIN_EXE_comb")),
                &cfg_root,
                &cfg_root,
                &sock,
            )
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
         \x20 [PASS] daemon   parsed ok — 0 config Sources\n\
         \x20 [PASS] daemon   config parsed\n\
         \x20 [PASS] daemon   0 config Sources loaded\n\
         \x20 [PASS] local    config file valid\n\
         \x20 [PASS] local    defaults {} ({} files, comb {})\n\
         \n",
        "~/beachcomber/config.toml",
        "~/beachcomber/defaults",
        beachcomber::daemon::defaults::SHIPPED.len(),
        env!("BEACHCOMBER_VERSION"),
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        expected,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0), "all passed exits 0");
}

/// Two edits back to back, before the scheduler has put the first registry in
/// force: the second is adopted from the first, not from the registry the
/// handle still serves, so what the first carried is not lost. The scheduler is
/// stood in for by a queue the test reads the `Reload`s from.
#[test]
fn a_second_edit_adopts_from_the_registry_the_first_sent() {
    use beachcomber::provider::registry::RegistryHandle;
    use beachcomber::scheduler::{SchedulerHandle, SchedulerMessage};

    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("config.toml");
    let source = |name: &str| {
        format!(
            "[daemon.providers.p.sources.{name}]\ndata = \"{{{{ 1 }}}}\"\nresult.{name} = \"{{{{ data }}}}\"\n\
             return = \"{{{{ result | poll('10m') }}}}\"\n"
        )
    };
    let layers = |toml: String| vec![(cfg.clone(), toml::from_str::<toml::Value>(&toml).unwrap())];

    let cache = Arc::new(Cache::new());
    let first = Config::from_layers(layers(source("a")));
    let registry = ProviderRegistry::load(&first, &HashMap::new())
        .registry
        .adopt(&ProviderRegistry::new(), &cache);
    let handle = RegistryHandle::new(registry.clone());
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let sink = SchedulerHandle::new(tx);
    let mut last = registry;

    reload(
        &cache,
        &handle,
        &sink,
        &mut last,
        layers(source("a") + &source("b")),
    );
    let Ok(SchedulerMessage::Reload {
        next: first_sent, ..
    }) = rx.try_recv()
    else {
        panic!("the first edit sends a Reload");
    };
    // What the first registry has learned since: `a` was seen calling `b`.
    first_sent.set_callees(
        ("p", "a", None),
        HashMap::from([(("p".to_string(), "b".to_string()), vec![None])]),
    );
    assert!(
        Arc::ptr_eq(&handle.current(), &handle.current())
            && handle.current().callers_of().is_empty(),
        "the handle still serves the registry before the first edit"
    );

    reload(
        &cache,
        &handle,
        &sink,
        &mut last,
        layers(source("a") + &source("b") + &source("c")),
    );
    let Ok(SchedulerMessage::Reload {
        next: second_sent, ..
    }) = rx.try_recv()
    else {
        panic!("the second edit sends a Reload");
    };
    assert_eq!(
        second_sent.callers_of().len(),
        1,
        "the second registry carries what the first had learned"
    );
}
