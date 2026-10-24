//! Integration tests for the daemon's config-file self-watch
//! (`src/daemon/config_watch.rs`), which mirrors the binary self-watch
//! (`tests/suite/binary_self_watch.rs`) but only restarts when the changed file
//! parses cleanly.
//!
//! These use the polling backend (`fs_events: false`) so they run reliably
//! under sandboxed hosts that can't use FSEvents / inotify, same convention
//! as the watcher tests in `tests/`.

use crate::common::git::{GitRepoFixture, has_git};
use crate::common::mtime::touch_future;
use crate::common::poll::poll_until_async;
use crate::seed::{Env, rewrite, send_recv};
use beachcomber::daemon::config_watch::{ChangeKind, spawn_config_self_watch_with};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

/// Spawn a watch over `cfg` polling every `ms` milliseconds with the fs-event
/// fast path off, and hand back its thread plus a channel of the changes it
/// reports. Every test here wants exactly this; only the two that pin the stop
/// token or the change kind look past the receiver.
fn watch(
    cfg: PathBuf,
    ms: u64,
) -> (
    std::thread::JoinHandle<()>,
    std::sync::mpsc::Receiver<ChangeKind>,
) {
    watch_until(cfg, ms, CancellationToken::new())
}

/// [`watch`] with a caller-owned stop token.
fn watch_until(
    cfg: PathBuf,
    ms: u64,
    stop: CancellationToken,
) -> (
    std::thread::JoinHandle<()>,
    std::sync::mpsc::Receiver<ChangeKind>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = spawn_config_self_watch_with(
        cfg,
        Duration::from_millis(ms),
        false, // fs events off — the mtime poll is the mechanism under test
        stop,
        move |kind, _, _| {
            let _ = tx.send(kind);
        },
    );
    (handle, rx)
}

#[test]
fn poll_restarts_on_valid_config_change_when_fs_events_are_dead() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();

    let (_watch, rx) = watch(cfg.clone(), 100);

    std::fs::write(&cfg, "[daemon]\nlog_level = \"debug\"\n").unwrap();
    touch_future(&cfg);

    rx.recv_timeout(Duration::from_secs(3))
        .expect("valid config change triggered restart");
}

#[test]
fn poll_ignores_invalid_config_change_and_keeps_running() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();

    let (_watch, rx) = watch(cfg.clone(), 100);

    // Not valid TOML.
    std::fs::write(&cfg, "this is not [ valid toml").unwrap();
    touch_future(&cfg);

    // Give the poll several intervals to notice and (wrongly) fire; it must not.
    let result = rx.recv_timeout(Duration::from_millis(800));
    assert!(
        result.is_err(),
        "daemon must not restart into a config that fails to parse"
    );
}

#[test]
fn config_file_created_after_daemon_start_is_picked_up() {
    // Spec item: "the daemon may have started with no config file — handle
    // the file appearing later if the existing watch machinery makes that
    // natural". Here it's natural: the poll stats the path regardless of
    // whether anything exists there yet.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml"); // does not exist yet

    let (_watch, rx) = watch(cfg.clone(), 100);

    std::fs::write(&cfg, "[daemon]\n").unwrap();
    touch_future(&cfg);

    rx.recv_timeout(Duration::from_secs(3))
        .expect("config file created after startup should be picked up");
}

#[test]
fn config_file_that_never_appears_never_restarts() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("never-written.toml");

    let (_watch, rx) = watch(cfg.clone(), 100);

    let result = rx.recv_timeout(Duration::from_millis(500));
    assert!(
        result.is_err(),
        "no config file ever appeared; daemon must not restart"
    );
}

// ── conf.d coverage ──────────────────────────────────────────────────────

#[test]
fn conf_d_file_change_triggers_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();
    let conf_d = tmp.path().join("conf.d");
    std::fs::create_dir(&conf_d).unwrap();
    let drop_in = conf_d.join("01-drop-in.toml");
    std::fs::write(&drop_in, "[daemon]\nlog_level = \"debug\"\n").unwrap();

    let (_watch, rx) = watch(cfg.clone(), 100);

    std::fs::write(&drop_in, "[daemon]\nlog_level = \"trace\"\n").unwrap();
    touch_future(&drop_in);

    rx.recv_timeout(Duration::from_secs(3))
        .expect("conf.d drop-in change should trigger restart");
}

#[test]
fn conf_d_invalid_edit_does_not_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();
    let conf_d = tmp.path().join("conf.d");
    std::fs::create_dir(&conf_d).unwrap();
    let drop_in = conf_d.join("01-drop-in.toml");
    std::fs::write(&drop_in, "[daemon]\nlog_level = \"debug\"\n").unwrap();

    let (_watch, rx) = watch(cfg.clone(), 100);

    std::fs::write(&drop_in, "this is not [ valid toml").unwrap();
    touch_future(&drop_in);

    let result = rx.recv_timeout(Duration::from_millis(800));
    assert!(
        result.is_err(),
        "daemon must not restart when a conf.d drop-in fails to parse"
    );
}

#[test]
fn conf_d_only_change_restarts_without_a_main_config_file() {
    // A daemon whose config lives entirely in conf.d/ (no config.toml at
    // all) must still restart on a conf.d change. Validation must treat a
    // missing main config file as empty content — same tolerance `Config::load()`
    // already gives daemon startup — not as a failure that blocks every restart.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml"); // deliberately never created
    let conf_d = tmp.path().join("conf.d");
    std::fs::create_dir(&conf_d).unwrap();
    let drop_in = conf_d.join("01-drop-in.toml");
    std::fs::write(&drop_in, "[daemon]\nlog_level = \"debug\"\n").unwrap();

    let (_watch, rx) = watch(cfg.clone(), 100);

    std::fs::write(&drop_in, "[daemon]\nlog_level = \"trace\"\n").unwrap();
    touch_future(&drop_in);

    rx.recv_timeout(Duration::from_secs(3))
        .expect("conf.d change should trigger restart even when config.toml doesn't exist");
}

#[test]
fn new_file_appearing_in_conf_d_triggers_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();
    let conf_d = tmp.path().join("conf.d");
    std::fs::create_dir(&conf_d).unwrap();

    let (_watch, rx) = watch(cfg.clone(), 100);

    let new_file = conf_d.join("02-new.toml");
    std::fs::write(&new_file, "[daemon]\nlog_level = \"warn\"\n").unwrap();
    touch_future(&new_file);

    rx.recv_timeout(Duration::from_secs(3))
        .expect("a new conf.d file should trigger restart (file-count delta)");
}

// ── re-arming and change classification ──────────────────────────────────────

#[test]
fn the_watch_re_arms_and_reports_every_change() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();

    let (_watch, rx) = watch(cfg.clone(), 50);

    std::fs::write(&cfg, "[daemon]\nlog_level = \"debug\"\n").unwrap();
    touch_future(&cfg);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3)),
        Ok(ChangeKind::Settings),
        "first change"
    );

    std::fs::write(&cfg, "[daemon]\nlog_level = \"trace\"\n").unwrap();
    touch_future(&cfg);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3)),
        Ok(ChangeKind::Settings),
        "the watch re-arms: a second change is reported too"
    );
}

#[test]
fn a_client_template_edit_reseeds_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(
        &cfg,
        "[daemon]\nlog_level = \"debug\"\n\n[client.providers.x]\ntemplate.a = \"1\"\n",
    )
    .unwrap();

    let (_watch, rx) = watch(cfg.clone(), 50);

    std::fs::write(
        &cfg,
        "[daemon]\nlog_level = \"debug\"\n\n[client.providers.x]\ntemplate.a = \"2\"\n",
    )
    .unwrap();
    touch_future(&cfg);

    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3)),
        Ok(ChangeKind::Providers),
        "only a client template changed — the daemon reseeds instead of restarting"
    );
}

#[test]
fn an_edit_is_seen_behind_another_file_stamped_into_the_future() {
    // One "newest mtime" reference point cannot see past a file whose mtime is
    // far ahead — a network mount's clock skew, an editor, a test. Per-file
    // mtimes can: only the file that actually moved compares unequal.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();
    let conf_d = tmp.path().join("conf.d");
    std::fs::create_dir(&conf_d).unwrap();

    let ahead = conf_d.join("01-ahead.toml");
    std::fs::write(&ahead, "[daemon]\n").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&ahead)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(3600))
        .unwrap();

    let other = conf_d.join("02-other.toml");
    std::fs::write(&other, "[daemon]\nlog_level = \"debug\"\n").unwrap();

    let (_watch, rx) = watch(cfg.clone(), 50);

    std::fs::write(&other, "[daemon]\nlog_level = \"trace\"\n").unwrap();
    touch_future(&other);

    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3)),
        Ok(ChangeKind::Settings),
        "an edit to one file must be seen even while another is stamped an hour ahead"
    );
    assert!(
        rx.recv_timeout(Duration::from_millis(400)).is_err(),
        "and the file stamped ahead must not re-trigger on every tick"
    );
}

#[test]
fn cancelling_the_stop_token_ends_the_watch_thread() {
    // The watch's closure holds the daemon's cache and registry, so a watch
    // that outlives its daemon is a leak rather than a process exit.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();

    let stop = CancellationToken::new();
    let (thread, rx) = watch_until(cfg.clone(), 50, stop.clone());

    // Alive and reporting first, so the silence below means something.
    std::fs::write(&cfg, "[daemon]\nlog_level = \"debug\"\n").unwrap();
    touch_future(&cfg);
    rx.recv_timeout(Duration::from_secs(3))
        .expect("the watch reports while it is running");

    stop.cancel();

    // `JoinHandle::join` has no timeout of its own; bound it through a channel
    // so a watch that never exits fails the test instead of hanging it.
    let (joined_tx, joined_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = thread.join();
        let _ = joined_tx.send(());
    });
    joined_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("the watch thread exits once its stop token is cancelled");

    std::fs::write(&cfg, "[daemon]\nlog_level = \"trace\"\n").unwrap();
    touch_future(&cfg);
    assert!(
        rx.recv_timeout(Duration::from_millis(400)).is_err(),
        "a stopped watch must report nothing"
    );
}

// ── a Source that would not load ────────────────────────────────────────────

/// The gate that keeps the daemon up on a bad edit is unit-tested in
/// `config_watch.rs` (`try_fire` is synchronous); this is the positive event,
/// through the real watch: a Source file that loads is a providers change.
#[test]
fn a_fixed_return_in_conf_d_reloads() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(
        &cfg,
        "[daemon.providers.demo.sources.s]\ndata = \"{{ 1 }}\"\nresult = \"{{ data }}\"\n",
    )
    .unwrap();
    let conf_d = tmp.path().join("conf.d");
    std::fs::create_dir_all(&conf_d).unwrap();
    let mine = conf_d.join("10-mine.toml");

    let (_watch, rx) = watch(cfg.clone(), 50);

    std::fs::write(
        &mine,
        "[daemon.providers.demo.sources.s]\nreturn = \"{{ result | poll('9s') }}\"\n",
    )
    .unwrap();
    touch_future(&mine);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3)),
        Ok(ChangeKind::Providers)
    );
}

// ── an edit under providers reloads the running daemon ──────────────────────

/// An in-process daemon over a config directory the test edits, and the one
/// connection it talks over. "The same daemon" is: the same connection still
/// answers, the same process id, its shutdown token never cancelled (a restart
/// is that token being cancelled) and its task still running.
struct Live {
    env: Env,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    s: UnixStream,
    pid: u64,
}

const RETUNED: &str = "[daemon.providers.git.sources.diff]\n\
                       return = \"{{ result | poll('15s', 8) | nearest }}\"\n";

fn retuned_poll() -> Value {
    json!({"interval": "15s", "count": 8})
}

/// A Source of its own: one field, `demo.n`, polled every 5s.
const DEMO: &str = "[daemon.providers.demo.sources.s]\ndata = \"{{ 7 }}\"\n\
                    result.n = \"{{ data }}\"\nreturn = \"{{ result | poll('5s') }}\"\n";

impl Live {
    async fn start(main: &str, drop_ins: &[(&str, &str)]) -> Live {
        let env = Env::new(main, drop_ins);
        let cancel = CancellationToken::new();
        let (task, ready) = env.start_with(cancel.clone());
        ready.await.expect("daemon ready");
        let mut s = env.connect().await;
        let pid = pid_of(&mut s).await;
        Live {
            env,
            cancel,
            task,
            s,
            pid,
        }
    }

    /// The `data` of a `get`, `Null` on a miss.
    async fn value(&mut self, key: &str, path: Option<&str>) -> Value {
        let resp = send_recv(&mut self.s, json!({"op": "get", "key": key, "path": path})).await;
        resp.get("data").cloned().unwrap_or(Value::Null)
    }

    /// Poll until `key` reads as `want`: the positive event a reload is.
    async fn until(&mut self, what: &str, key: &str, path: Option<&str>, want: Value) {
        poll_until_async(
            what,
            &format!("{key} to read {want}"),
            Duration::from_secs(10),
            Duration::from_millis(20),
            async || self.value(key, path).await,
            |v| *v == want,
        )
        .await;
    }

    /// How many rows `comb status` shows for `provider`.
    async fn rows(&mut self, provider: &str) -> usize {
        let resp = send_recv(&mut self.s, json!({"op": "status"})).await;
        resp["data"].as_array().map_or(0, |rows| {
            rows.iter()
                .filter(|r| r["provider"] == json!(provider))
                .count()
        })
    }

    async fn until_no_rows(&mut self, provider: &str) {
        poll_until_async(
            "status",
            &format!("no rows left for {provider}"),
            Duration::from_secs(10),
            Duration::from_millis(20),
            async || self.rows(provider).await,
            |n| *n == 0,
        )
        .await;
    }

    /// Poll until the scheduler holds a lifecycle entry for `provider`'s
    /// `source`: a reload compares entries, so there has to be one.
    async fn until_demanded(&mut self, provider: &str, source: &str) {
        poll_until_async(
            "introspect demand",
            &format!("an entry for {provider}.{source}"),
            Duration::from_secs(10),
            Duration::from_millis(20),
            async || {
                let resp = send_recv(
                    &mut self.s,
                    json!({"op": "introspect", "subject": "demand"}),
                )
                .await;
                resp["data"]["demand"].as_array().is_some_and(|d| {
                    d.iter()
                        .any(|e| e["provider"] == json!(provider) && e["source"] == json!(source))
                })
            },
            |held| *held,
        )
        .await;
    }

    /// The daemon never restarted: same connection, same pid, token and task
    /// untouched.
    async fn assert_same_daemon(&mut self) {
        assert!(
            !self.cancel.is_cancelled(),
            "the daemon was told to shut down"
        );
        assert!(!self.task.is_finished(), "the daemon task ended");
        assert_eq!(pid_of(&mut self.s).await, self.pid, "same daemon");
    }
}

async fn pid_of(s: &mut UnixStream) -> u64 {
    let resp = send_recv(s, json!({"op": "introspect", "subject": "daemon"})).await;
    resp["data"]["pid"].as_u64().expect("introspect daemon pid")
}

/// The stored `poll` of a record, from its `:cache` leaf.
async fn poll_of(live: &mut Live, key: &str, path: &str) -> Value {
    live.value(key, Some(path))
        .await
        .get("poll")
        .cloned()
        .unwrap_or(Value::Null)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editing_a_return_line_reloads_without_restart() {
    if !has_git() {
        return;
    }
    let mut live = Live::start("", &[]).await;
    let repo = GitRepoFixture::new();
    let p = repo.path_str();
    assert_ne!(live.value("git.lines_added", Some(p)).await, Value::Null);
    live.until_demanded("git", "diff").await;
    assert_ne!(
        poll_of(&mut live, "git:cache.lines_added", p).await,
        retuned_poll(),
        "the shipped schedule is not the one about to be set"
    );

    rewrite(&live.env.drop_in("10-mine.toml"), RETUNED);
    poll_until_async(
        "git:cache.lines_added",
        "the new poll, 15s x 8",
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || poll_of(&mut live, "git:cache.lines_added", p).await,
        |poll| *poll == retuned_poll(),
    )
    .await;
    live.assert_same_daemon().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adding_a_source_in_conf_d_reloads() {
    let mut live = Live::start("", &[]).await;
    assert_eq!(
        live.value("demo.n", None).await,
        Value::Null,
        "no such provider yet"
    );

    rewrite(&live.env.drop_in("10-demo.toml"), DEMO);
    live.until("demo.n", "demo.n", None, json!(7)).await;
    live.assert_same_daemon().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_a_source_evicts_its_records() {
    let mut live = Live::start("", &[("10-demo.toml", DEMO)]).await;
    live.until("demo.n", "demo.n", None, json!(7)).await;
    live.until_demanded("demo", "s").await;
    assert!(live.rows("demo").await > 0, "the Source's record is stored");

    std::fs::remove_file(live.env.drop_in("10-demo.toml")).unwrap();
    live.until_no_rows("demo").await;
    assert_eq!(live.value("demo.n", None).await, Value::Null);
    assert_eq!(live.value("demo:cache.n", None).await, Value::Null);
    let providers = send_recv(
        &mut live.s,
        json!({"op": "introspect", "subject": "providers"}),
    )
    .await;
    assert!(
        !providers["data"]["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == json!("demo")),
        "a provider no longer named is no longer registered: {providers}"
    );
    live.assert_same_daemon().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broken_providers_edit_changes_nothing() {
    if !has_git() {
        return;
    }
    let mut live = Live::start("", &[]).await;
    let repo = GitRepoFixture::new();
    let p = repo.path_str();
    assert_ne!(live.value("git.lines_added", Some(p)).await, Value::Null);
    live.until_demanded("git", "diff").await;

    // Not a duration: the gate refuses it, and the daemon keeps what it has.
    let mine = live.env.drop_in("10-mine.toml");
    rewrite(
        &mine,
        "[daemon.providers.git.sources.diff]\nreturn = \"{{ result | poll('banana') }}\"\n",
    );
    // The loader the gate runs refuses exactly these files, so the edit the
    // watch sees is one it declines.
    let config_file = live.env.cfg_dir.join("config.toml");
    let refused = beachcomber::provider::registry::ProviderRegistry::load(
        &beachcomber::config::Config::load_from(&config_file),
        &Default::default(),
    )
    .errors;
    assert!(
        refused.iter().any(|e| e.contains("banana")),
        "the broken edit is refused: {refused:?}"
    );
    // The fix is the positive event: the daemon took it by reloading, not by
    // restarting, and it never took the broken one.
    rewrite(&mine, RETUNED);
    poll_until_async(
        "git:cache.lines_added",
        "the fixed poll, 15s x 8",
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || poll_of(&mut live, "git:cache.lines_added", p).await,
        |poll| *poll == retuned_poll(),
    )
    .await;
    live.assert_same_daemon().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bare_daemon_key_still_restarts() {
    let live = Live::start("", &[]).await;
    rewrite(
        &live.env.drop_in("10-log.toml"),
        "[daemon]\nlog_level = \"debug\"\n",
    );
    let bound = Duration::from_secs(10);
    tokio::time::timeout(bound, live.cancel.cancelled())
        .await
        .expect("the daemon was told to shut down for a restart");
    tokio::time::timeout(bound, live.task)
        .await
        .expect("the daemon task ends")
        .expect("without panicking");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decay_table_edit_reseeds_in_place() {
    let mut live = Live::start(
        "",
        &[(
            "10-decay.toml",
            "[daemon.providers.git.cache._]\ndecay = 2\n",
        )],
    )
    .await;
    live.until("git:cache._.decay", "git:cache._.decay", None, json!(2))
        .await;

    rewrite(
        &live.env.drop_in("10-decay.toml"),
        "[daemon.providers.git.cache._]\ndecay = 3\n",
    );
    live.until("git:cache._.decay", "git:cache._.decay", None, json!(3))
        .await;
    live.assert_same_daemon().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabling_a_provider_reloads_and_evicts_its_instances() {
    if !has_git() {
        return;
    }
    let mut live = Live::start("", &[]).await;
    let repo = GitRepoFixture::new();
    let p = repo.path_str();
    assert_eq!(live.value("git.branch", Some(p)).await, json!("main"));
    live.until_demanded("git", "head").await;
    assert!(live.rows("git").await > 0);

    rewrite(
        &live.env.drop_in("10-off.toml"),
        "[daemon.providers.git]\nenabled = false\n",
    );
    live.until_no_rows("git").await;
    assert_eq!(live.value("git.branch", Some(p)).await, Value::Null);
    live.assert_same_daemon().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reload_clears_kept_roots() {
    // Each render of the root appends a line to a counter, so the count is how
    // many times the root was worked out.
    let mut live = Live::start("", &[]).await;
    let home = std::env::var("HOME").unwrap();
    let counter = std::path::Path::new(&home).join("roots");
    let proj = std::path::Path::new(&home).join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let source = r#"
[daemon.providers.demo.sources.s]
root = "{{ exec('echo run >> ' ~ env.HOME ~ '/roots; echo ' ~ env.HOME ~ '/proj') | trim }}"
data = "{{ 1 }}"
result.n = "{{ data }}"
"#;
    let lines = || std::fs::read_to_string(&counter).map_or(0, |text| text.lines().count());
    rewrite(&live.env.drop_in("10-demo.toml"), source);
    let at = proj.to_str().unwrap();
    live.until("demo.n", "demo.n", Some(at), json!(1)).await;
    assert_eq!(lines(), 1, "the root is rendered once for the directory");
    assert_eq!(live.value("demo.n", Some(at)).await, json!(1));
    assert_eq!(lines(), 1, "and kept");

    // An edit that touches no Source still replaces the registry, and with it
    // the roots it kept; the template is the witness that the reload ran.
    rewrite(
        &live.env.drop_in("20-t.toml"),
        "[client.providers.demo]\ntemplate.a = \"1\"\n",
    );
    live.until("demo:template.a", "demo:template.a", None, json!("1"))
        .await;
    assert_eq!(live.value("demo.n", Some(at)).await, json!(1));
    assert_eq!(lines(), 2, "a reload forgets the kept roots");
    live.assert_same_daemon().await;
}

// ── what a reload does to work already under way ────────────────────────────

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap())
}

/// A Source whose body waits for `$HOME/go` to exist, then reads `$HOME/val`,
/// so a test decides when a run is allowed to finish.
fn gated(poll: &str) -> String {
    format!(
        "[daemon.providers.demo.sources.s]\n\
         data = \"{{{{ exec('while [ ! -f $HOME/go ]; do sleep 0.05; done; cat $HOME/val') | trim }}}}\"\n\
         result.n = \"{{{{ data }}}}\"\n\
         return = \"{{{{ result | poll('{poll}') }}}}\"\n"
    )
}

impl Live {
    /// Runs the scheduler holds right now.
    async fn in_flight(&mut self) -> u64 {
        let resp = send_recv(
            &mut self.s,
            json!({"op": "introspect", "subject": "daemon"}),
        )
        .await;
        resp["data"]["in_flight"].as_u64().expect("in_flight")
    }

    async fn until_in_flight(&mut self, want: u64) {
        poll_until_async(
            "introspect daemon",
            &format!("{want} run(s) in flight"),
            Duration::from_secs(10),
            Duration::from_millis(20),
            async || self.in_flight().await,
            |n| *n == want,
        )
        .await;
    }

    /// Whether `introspect providers` lists `name`: the registry in force.
    async fn lists(&mut self, name: &str) -> bool {
        let resp = send_recv(
            &mut self.s,
            json!({"op": "introspect", "subject": "providers"}),
        )
        .await;
        resp["data"]["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == json!(name))
    }

    async fn until_listed(&mut self, name: &str, listed: bool) {
        poll_until_async(
            "introspect providers",
            &format!("{name} listed: {listed}"),
            Duration::from_secs(10),
            Duration::from_millis(20),
            async || self.lists(name).await,
            |l| *l == listed,
        )
        .await;
    }

    /// The stored value of `provider.field` as `comb status` shows it, which
    /// reads the cache and so asks nothing of the scheduler.
    async fn stored(&mut self, provider: &str, field: &str) -> Option<Value> {
        let resp = send_recv(&mut self.s, json!({"op": "status"})).await;
        resp["data"].as_array()?.iter().find_map(|r| {
            (r["provider"] == json!(provider) && r["field"] == json!(field))
                .then(|| r["value"].clone())
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_spanning_a_reload_still_reads_its_siblings() {
    let t = "[daemon.providers.demo.sources.t]\ndata = \"{{ 5 }}\"\nresult.v = \"{{ data }}\"\n\
             return = \"{{ result | poll('10m') }}\"\n";
    let s = gated("10m").replace(
        "| trim }}\"\nresult.n",
        "| trim }}-{{ sources.t().v }}\"\nresult.n",
    );
    let mut live = Live::start("", &[("10-demo.toml", &format!("{t}{s}"))]).await;
    std::fs::write(home().join("val"), "a").unwrap();
    std::fs::write(home().join("go"), "").unwrap();
    assert_eq!(live.value("demo.n", None).await, json!("a-5"));

    // A run under way when an unrelated edit reloads the daemon.
    std::fs::remove_file(home().join("go")).unwrap();
    std::fs::write(home().join("val"), "b").unwrap();
    send_recv(&mut live.s, json!({"op": "refresh", "key": "demo.n"})).await;
    live.until_in_flight(1).await;
    rewrite(
        &live.env.drop_in("20-z.toml"),
        "[client.providers.zzz]\ntemplate.a = \"1\"\n",
    );
    live.until_listed("zzz", true).await;

    std::fs::write(home().join("go"), "").unwrap();
    live.until_in_flight(0).await;
    assert_eq!(
        live.value("demo.n", None).await,
        json!("b-5"),
        "the run still reads the Source it calls"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_sources_run_in_flight_leaves_no_record() {
    let mut live = Live::start("", &[("10-demo.toml", &gated("10m"))]).await;
    std::fs::write(home().join("val"), "1").unwrap();
    std::fs::write(home().join("go"), "").unwrap();
    assert_eq!(live.value("demo.n", None).await, json!("1"));

    std::fs::remove_file(home().join("go")).unwrap();
    send_recv(&mut live.s, json!({"op": "refresh", "key": "demo.n"})).await;
    live.until_in_flight(1).await;
    std::fs::remove_file(live.env.drop_in("10-demo.toml")).unwrap();
    live.until_listed("demo", false).await;
    live.until_no_rows("demo").await;

    std::fs::write(home().join("go"), "").unwrap();
    live.until_in_flight(0).await;
    assert_eq!(
        live.rows("demo").await,
        0,
        "a run that finished after its Source went does not write it back"
    );
}

/// A `comb refresh` of an instance nothing has read yet has no lifecycle entry
/// to mark: the edit that changes its body must still keep the run's result
/// out, or a Source that only a watch re-runs serves it until a file changes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refresh_in_flight_with_no_entry_leaves_no_result_of_the_old_body() {
    let watch_only = |suffix: &str| {
        gated("10m")
            .replace("poll('10m')", "watch('/etc/hosts')")
            .replace(
                "result.n = \"{{ data }}\"",
                &format!("result.n = \"{{{{ data }}}}{suffix}\""),
            )
    };
    let mut live = Live::start("", &[("10-demo.toml", &watch_only(""))]).await;
    std::fs::write(home().join("val"), "1").unwrap();
    send_recv(&mut live.s, json!({"op": "refresh", "key": "demo.n"})).await;
    live.until_in_flight(1).await;

    rewrite(&live.env.drop_in("10-demo.toml"), &watch_only("-new"));
    // The edit's reload is in force once the registry's template is.
    rewrite(
        &live.env.drop_in("20-z.toml"),
        "[client.providers.zzz]\ntemplate.a = \"1\"\n",
    );
    live.until_listed("zzz", true).await;

    std::fs::write(home().join("go"), "").unwrap();
    live.until_in_flight(0).await;
    assert_eq!(
        live.rows("demo").await,
        0,
        "the run began under the old body and left nothing behind"
    );
    assert_eq!(
        live.value("demo.n", None).await,
        json!("1-new"),
        "the next read runs the body in force"
    );
}

/// A Source whose `data` line called a disabled provider's Source stored the
/// miss it got. Switching the provider on again changes what that call gives,
/// so the caller's record goes and the next read computes it afresh.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn re_enabling_a_provider_recomputes_the_sources_that_called_it() {
    let cfg = |enabled: bool| {
        format!(
            "[daemon.providers.off]\nenabled = {enabled}\n\n\
             [daemon.providers.off.sources.s]\ndata = \"{{{{ 1 }}}}\"\nresult.v = \"{{{{ data }}}}\"\n\
             return = \"{{{{ result | poll('10m') }}}}\"\n\n\
             [daemon.providers.viadata.sources.own]\n\
             data = \"{{{{ 'miss' if daemon.providers.off.sources.s() is none else 'hit' }}}}\"\n\
             result.seen = \"{{{{ data }}}}\"\nreturn = \"{{{{ result | poll('10m') }}}}\"\n"
        )
    };
    let mut live = Live::start("", &[("10-demo.toml", &cfg(false))]).await;
    assert_eq!(
        live.value("viadata.seen", None).await,
        json!("miss"),
        "the call to a disabled provider is a miss"
    );

    rewrite(&live.env.drop_in("10-demo.toml"), &cfg(true));
    live.until("viadata.seen", "viadata.seen", None, json!("hit"))
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rescheduled_sources_run_in_flight_does_not_restore_the_old_schedule() {
    let mut live = Live::start("", &[("10-demo.toml", &gated("10m"))]).await;
    std::fs::write(home().join("val"), "1").unwrap();
    std::fs::write(home().join("go"), "").unwrap();
    assert_eq!(live.value("demo.n", None).await, json!("1"));
    live.until_demanded("demo", "s").await;

    std::fs::remove_file(home().join("go")).unwrap();
    send_recv(&mut live.s, json!({"op": "refresh", "key": "demo.n"})).await;
    live.until_in_flight(1).await;
    rewrite(&live.env.drop_in("10-demo.toml"), &gated("11m"));
    poll_until_async(
        "demo:cache.n",
        "the new poll, stored at the reload",
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || live.value("demo:cache.n", None).await["poll"]["interval"].clone(),
        |interval| *interval == json!("660s"),
    )
    .await;

    std::fs::write(home().join("go"), "").unwrap();
    live.until_in_flight(0).await;
    assert_eq!(
        live.value("demo:cache.n", None).await["poll"]["interval"],
        json!("660s"),
        "the run that was under way did not put the old schedule back"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edited_body_serves_the_new_value_at_once() {
    let body = |n: u32| {
        format!(
            "[daemon.providers.demo.sources.s]\ndata = \"{{{{ {n} }}}}\"\n\
             result.n = \"{{{{ data }}}}\"\nreturn = \"{{{{ result | poll('10m') }}}}\"\n"
        )
    };
    let mut live = Live::start("", &[("10-demo.toml", &body(7))]).await;
    assert_eq!(live.value("demo.n", None).await, json!(7));

    rewrite(&live.env.drop_in("10-demo.toml"), &body(8));
    live.until("demo.n", "demo.n", None, json!(8)).await;
    live.assert_same_daemon().await;
}

/// A Source that counts its runs in `$HOME/runs`.
fn counted(ret: &str) -> String {
    format!(
        "[daemon.providers.demo.sources.s]\n\
         data = \"{{{{ exec('echo run >> $HOME/runs; cat $HOME/val') | trim }}}}\"\n\
         result.n = \"{{{{ data }}}}\"\nreturn = \"{{{{ result | {ret} }}}}\"\n"
    )
}

fn runs() -> usize {
    std::fs::read_to_string(home().join("runs")).map_or(0, |t| t.lines().count())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retune_keeps_the_instance_and_restamps_its_schedule_without_a_run() {
    let mut live = Live::start("", &[("10-demo.toml", &counted("poll('10m')"))]).await;
    std::fs::write(home().join("val"), "a").unwrap();
    assert_eq!(live.value("demo.n", None).await, json!("a"));
    live.until_demanded("demo", "s").await;
    assert_eq!(runs(), 1);

    rewrite(&live.env.drop_in("10-demo.toml"), &counted("poll('11m')"));
    poll_until_async(
        "demo:cache.n",
        "the new poll",
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || live.value("demo:cache.n", None).await["poll"]["interval"].clone(),
        |interval| *interval == json!("660s"),
    )
    .await;
    live.until_demanded("demo", "s").await;
    assert_eq!(runs(), 1, "restamped, not run again");
    assert_eq!(live.value("demo.n", None).await, json!("a"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retuned_sources_new_watch_fires_without_a_new_demand() {
    let mut live = Live::start("", &[("10-demo.toml", &counted("poll('10m')"))]).await;
    std::fs::write(home().join("val"), "a").unwrap();
    assert_eq!(live.value("demo.n", None).await, json!("a"));
    live.until_demanded("demo", "s").await;

    // Now invalidated by a file, not a clock. From here nothing asks for the
    // value but `status`, which reads the cache and demands nothing.
    let watched = home().join("val");
    rewrite(
        &live.env.drop_in("10-demo.toml"),
        &counted(&format!("watch('{}')", watched.display())),
    );
    poll_until_async(
        "demo:cache.n",
        "the Source to be watched",
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || live.value("demo:cache.n", None).await.get("watch").cloned(),
        |w| w.is_some(),
    )
    .await;
    std::fs::write(&watched, "b").unwrap();
    poll_until_async(
        "status demo.n",
        "the watch to refresh the Source",
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || live.stored("demo", "n").await,
        |v| *v == Some(json!("b")),
    )
    .await;
    live.assert_same_daemon().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_follows_the_providers_across_a_reload() {
    let live = Live::start("", &[("10-demo.toml", DEMO)]).await;
    let mut w = live.env.connect().await;
    let (r, mut wr) = w.split();
    wr.write_all(b"{\"op\":\"watch\",\"key\":\"demo\"}\n")
        .await
        .unwrap();
    let mut lines = tokio::io::BufReader::new(r).lines();
    let mut frame = async || -> Value {
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .expect("a frame in time")
            .unwrap()
            .expect("the watch is open");
        serde_json::from_str(&line).unwrap()
    };
    while frame().await["data"] != json!({"n": 7}) {}

    // A second Source of the same provider: a plan made before it cannot know it.
    let both = format!(
        "{DEMO}[daemon.providers.demo.sources.t]\ndata = \"{{{{ 9 }}}}\"\n\
         result.m = \"{{{{ data }}}}\"\nreturn = \"{{{{ result | poll('5s') }}}}\"\n"
    );
    rewrite(&live.env.drop_in("10-demo.toml"), &both);
    while frame().await["data"] != json!({"n": 7, "m": 9}) {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_providers_puts_stay_reachable() {
    let mut live = Live::start("", &[("10-demo.toml", DEMO)]).await;
    live.until("demo.n", "demo.n", None, json!(7)).await;
    let put = send_recv(
        &mut live.s,
        json!({"op": "put", "key": "demo.x", "data": 1}),
    )
    .await;
    assert_eq!(put["ok"], json!(true), "{put}");

    std::fs::remove_file(live.env.drop_in("10-demo.toml")).unwrap();
    poll_until_async(
        "demo.n",
        "the Source's field gone",
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || live.value("demo.n", None).await,
        |v| v.is_null(),
    )
    .await;
    assert_eq!(live.value("demo.x", None).await, json!(1), "the put stays");
    assert!(
        live.lists("demo").await,
        "and its namespace is still listed"
    );
}

// ── what a reload does to work that began under the old registry ────────────

/// [`gated`], but its body leaves `$HOME/started` once it has begun.
fn started_gated() -> String {
    gated("10m").replace("exec('while", "exec('touch $HOME/started; while")
}

fn constant(n: u32) -> String {
    format!(
        "[daemon.providers.demo.sources.s]\ndata = \"{{{{ {n} }}}}\"\n\
         result.n = \"{{{{ data }}}}\"\nreturn = \"{{{{ result | poll('10m') }}}}\"\n"
    )
}

impl Live {
    /// Send `req` on a connection of its own and hand back the pending answer:
    /// a request-path run that a test holds open.
    fn request_in_background(&self, req: Value) -> tokio::task::JoinHandle<Value> {
        let sock = self.env.sock.clone();
        tokio::spawn(async move {
            let mut c = UnixStream::connect(sock).await.unwrap();
            send_recv(&mut c, req).await
        })
    }

    async fn until_started(&self) {
        poll_until_async(
            "started",
            "the held run to have begun",
            Duration::from_secs(10),
            Duration::from_millis(20),
            async || home().join("started").exists(),
            |began| *began,
        )
        .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cold_get_spanning_an_edit_does_not_store_the_old_body() {
    let mut live = Live::start("", &[("10-demo.toml", &started_gated())]).await;
    std::fs::write(home().join("val"), "1").unwrap();
    let held = live.request_in_background(json!({"op": "get", "key": "demo.n"}));
    live.until_started().await;

    rewrite(&live.env.drop_in("10-demo.toml"), &constant(2));
    rewrite(
        &live.env.drop_in("20-z.toml"),
        "[client.providers.zzz]\ntemplate.a = \"1\"\n",
    );
    live.until_listed("zzz", true).await;

    std::fs::write(home().join("go"), "").unwrap();
    held.await.unwrap();
    assert_eq!(
        live.value("demo.n", None).await,
        json!(2),
        "the run that began under the old body did not store its result"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forced_get_spanning_a_removal_leaves_no_row() {
    let mut live = Live::start("", &[("10-demo.toml", &started_gated())]).await;
    std::fs::write(home().join("val"), "1").unwrap();
    let held = live.request_in_background(json!({"op": "get", "key": "demo.n", "force": true}));
    live.until_started().await;

    std::fs::remove_file(live.env.drop_in("10-demo.toml")).unwrap();
    live.until_listed("demo", false).await;

    std::fs::write(home().join("go"), "").unwrap();
    held.await.unwrap();
    assert_eq!(
        live.rows("demo").await,
        0,
        "no orphan row of a removed Source"
    );

    rewrite(&live.env.drop_in("10-demo.toml"), &constant(2));
    live.until_listed("demo", true).await;
    assert_eq!(live.value("demo.n", None).await, json!(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fixed_body_is_not_held_back_by_the_failure_backoff_of_the_old_one() {
    let body = |data: &str| {
        format!(
            "[daemon.providers.demo.sources.s]\ndata = \"{{{{ {data} }}}}\"\n\
             result.n = \"{{{{ data }}}}\"\n\
             return = \"{{{{ result | poll('10m') | failback(1, '10m') }}}}\"\n"
        )
    };
    let mut live = Live::start("", &[("10-demo.toml", &body("exec('exit 1')"))]).await;
    // Two failures: into backoff, where a read no longer runs it.
    live.value("demo.n", None).await;
    live.value("demo.n", None).await;
    rewrite(&live.env.drop_in("10-demo.toml"), &body("5"));
    live.until("demo.n", "demo.n", None, json!(5)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_and_re_added_provider_is_not_a_static_namespace() {
    let mut live = Live::start("", &[("10-demo.toml", &constant(7))]).await;
    assert_eq!(live.value("demo.n", None).await, json!(7));
    send_recv(
        &mut live.s,
        json!({"op": "put", "key": "demo.x", "data": 1}),
    )
    .await;
    std::fs::remove_file(live.env.drop_in("10-demo.toml")).unwrap();
    live.until("demo.n", "demo.n", None, Value::Null).await;
    rewrite(&live.env.drop_in("10-demo.toml"), &constant(7));
    live.until("demo.n", "demo.n", None, json!(7)).await;

    let resp = send_recv(
        &mut live.s,
        json!({"op": "get", "key": "demo.n", "force": true}),
    )
    .await;
    assert_eq!(resp["ok"], json!(true), "{resp}");
    assert_eq!(resp["data"], json!(7));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_caller_of_a_changed_source_recomputes() {
    let cfg = |n: u32| {
        format!(
            "[daemon.providers.demo.sources.t]\ndata = \"{{{{ {n} }}}}\"\nresult.v = \"{{{{ data }}}}\"\n\
             return = \"{{{{ result | poll('10m') }}}}\"\n\n\
             [daemon.providers.demo.sources.s]\ndata = \"x-{{{{ sources.t().v }}}}\"\n\
             result.n = \"{{{{ data }}}}\"\nreturn = \"{{{{ result | poll('10m') }}}}\"\n"
        )
    };
    let mut live = Live::start("", &[("10-demo.toml", &cfg(5))]).await;
    assert_eq!(live.value("demo", None).await, json!({"n": "x-5", "v": 5}));
    rewrite(&live.env.drop_in("10-demo.toml"), &cfg(6));
    live.until("demo", "demo", None, json!({"n": "x-6", "v": 6}))
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_evicted_sources_overtaken_run_is_not_run_again() {
    let mut live = Live::start("", &[("10-demo.toml", &gated("10m"))]).await;
    std::fs::write(home().join("val"), "1").unwrap();
    std::fs::write(home().join("go"), "").unwrap();
    assert_eq!(live.value("demo.n", None).await, json!("1"));
    live.until_demanded("demo", "s").await;

    std::fs::remove_file(home().join("go")).unwrap();
    send_recv(&mut live.s, json!({"op": "refresh", "key": "demo.n"})).await;
    live.until_in_flight(1).await;
    let counted_body = gated("10m").replace("exec('while", "exec('echo run >> $HOME/runs; while");
    rewrite(&live.env.drop_in("10-demo.toml"), &counted_body);
    // The edit's reload is in force once the registry's template is.
    rewrite(
        &live.env.drop_in("20-z.toml"),
        "[client.providers.zzz]\ntemplate.a = \"1\"\n",
    );
    live.until_listed("zzz", true).await;

    std::fs::write(home().join("go"), "").unwrap();
    live.until_in_flight(0).await;
    // Round trips through the scheduler's queue, behind anything the run sent.
    for _ in 0..3 {
        live.in_flight().await;
    }
    assert_eq!(runs(), 0, "the overtaken run did not go again");
    assert_eq!(live.rows("demo").await, 0, "and left no row with no entry");
}
