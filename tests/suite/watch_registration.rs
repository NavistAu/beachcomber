//! A Source's watches are registered off the scheduler loop: each is Pending,
//! then Active or Failed, as `introspect watches` shows; a failed one is tried
//! again on the next demand; an absent path is watched through the nearest
//! directory above it that exists.

use crate::common::daemon::TestDaemon;
use crate::common::poll::poll_until;
use crate::common::socket::IsolatedSocket;
use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::scheduler::Scheduler;
use beachcomber::server::Server;
use beachcomber::watcher::FsWatcher;
use beachcomber::watcher_registry::WatcherRegistry;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// One request on a connection of its own.
fn ask(sock: &Path, request: Value) -> Value {
    let mut stream = UnixStream::connect(sock).expect("connect");
    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).unwrap();
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).unwrap();
    serde_json::from_str(&answer).expect("a JSON answer")
}

/// The watches the daemon reports, as `(path, the rest of the entry)`.
fn watches(sock: &Path) -> Vec<(String, Value)> {
    ask(sock, json!({"op": "introspect", "subject": "watches"}))["data"]["watches"]
        .as_array()
        .expect("watches")
        .iter()
        .map(|w| {
            let mut rest = w.clone();
            let path = rest
                .as_object_mut()
                .unwrap()
                .remove("path")
                .and_then(|p| p.as_str().map(str::to_string))
                .expect("a path");
            (path, rest)
        })
        .collect()
}

/// A config Source `w` with one instance, at `~`, whose `data` reads `file`
/// and which watches `watched`.
fn watching(file: &Path, watched: &Path) -> String {
    format!(
        "[daemon.providers.w.sources.s]\n\
         data = \"{{{{ '{}' | read }}}}\"\n\
         result.v = \"{{{{ data }}}}\"\n\
         return = \"{{{{ result | watch('{}') }}}}\"\n",
        file.display(),
        watched.display()
    )
}

/// A backend whose registrations wait until the test opens its gate.
struct Held(Arc<(Mutex<bool>, Condvar)>);

impl notify::Watcher for Held {
    fn new<F: notify::EventHandler>(_: F, _: notify::Config) -> notify::Result<Self> {
        Err(notify::Error::generic("made by the test"))
    }

    fn watch(&mut self, _: &Path, _: notify::RecursiveMode) -> notify::Result<()> {
        let (open, opened) = &*self.0;
        let mut open = open.lock().unwrap();
        while !*open {
            open = opened.wait(open).unwrap();
        }
        Ok(())
    }

    fn unwatch(&mut self, _: &Path) -> notify::Result<()> {
        Ok(())
    }

    fn kind() -> notify::WatcherKind {
        notify::WatcherKind::NullWatcher
    }
}

/// While a registration is held, `introspect watches` (which the scheduler
/// loop answers) shows it Pending, and the next read of the Source answers:
/// neither waits for it. Once the backend lets it through it is Active.
#[test]
fn a_held_registration_is_pending_and_waits_for_no_one() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let file = dir.join("state");
    std::fs::write(&file, "1").unwrap();
    let config = Config::from_toml(&watching(&file, &dir));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let sock = dir.join("s");
    {
        let _enter = rt.enter();
        let watchers = Arc::new(WatcherRegistry::new());
        let cache = Arc::new(Cache::with_watchers(watchers.clone()));
        let registry = ProviderRegistry::with_config(&config, &Default::default())
            .unwrap()
            .share(&cache);
        let (handle, mut scheduler) =
            Scheduler::new(cache.clone(), registry.clone(), config, watchers.clone());
        let (_events, rx) = tokio::sync::mpsc::channel(1);
        scheduler.use_watcher(FsWatcher::with_backend(Box::new(Held(gate.clone()))), rx);
        rt.spawn(scheduler.run());
        Server::new(sock.clone(), cache, registry, Some(handle), watchers)
            .spawn()
            .unwrap();
    }

    let get = json!({"op": "get", "key": "w.v"});
    assert_eq!(ask(&sock, get.clone())["data"], json!("1"), "the cold read");
    let path = dir.to_string_lossy().into_owned();
    let pending = (path.clone(), json!({"state": "pending"}));
    poll_until(
        "introspect watches",
        "the instance's watch, Pending",
        Duration::from_secs(10),
        Duration::from_millis(5),
        || watches(&sock),
        |w| *w == [pending.clone()],
    );
    assert_eq!(
        ask(&sock, get)["data"],
        json!("1"),
        "the next read answers with the registration held"
    );

    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    poll_until(
        "introspect watches",
        "the instance's watch, Active",
        Duration::from_secs(10),
        Duration::from_millis(5),
        || watches(&sock),
        |w| *w == [(path.clone(), json!({"state": "active"}))],
    );
}

/// The in-process daemon (polling watcher), its config `source`.
fn daemon(source: &str) -> TestDaemon {
    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    std::fs::create_dir_all(xdg.join("beachcomber")).unwrap();
    std::fs::write(xdg.join("beachcomber/config.toml"), source).unwrap();
    TestDaemon::serve_in_process(socket, home, xdg)
}

/// A path in a directory the daemon may not read cannot be watched: it is
/// Failed, `comb check daemon` warns of it, and the next read of the Source
/// asks for it again, which succeeds once the directory can be read.
#[test]
fn a_watch_that_fails_is_shown_and_tried_again_on_the_next_read() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let locked = tmp.path().canonicalize().unwrap().join("locked");
    std::fs::create_dir(&locked).unwrap();
    let file = locked.join("state");
    std::fs::write(&file, "1").unwrap();
    let d = daemon(&watching(&file, &file));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let unlock =
        || std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    if std::fs::metadata(&file).is_ok() {
        // Root reads through any mode: nothing here can fail.
        unlock();
        return;
    }
    let sock = d.socket.path.clone();
    let get = json!({"op": "get", "key": "w.v"});
    ask(&sock, get.clone());
    let path = file.to_string_lossy().into_owned();
    let failed = poll_until(
        "introspect watches",
        "the watch Failed",
        Duration::from_secs(10),
        Duration::from_millis(5),
        || watches(&sock),
        |w| w.len() == 1 && w[0].1["state"] == "failed",
    );
    assert_eq!(failed[0].0, path);
    assert!(
        failed[0].1["error"]
            .as_str()
            .is_some_and(|e| e.contains("ermission denied")),
        "{failed:?}"
    );
    let check = d
        .comb(tmp.path())
        .args(["check", "daemon"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&check.stdout);
    assert!(
        out.contains(&format!("[WARN] watch of {path} failed")),
        "comb check daemon shows the failure:\n{out}"
    );

    unlock();
    ask(&sock, get);
    poll_until(
        "introspect watches",
        "the watch Active after the next read",
        Duration::from_secs(10),
        Duration::from_millis(5),
        || watches(&sock),
        |w| *w == [(path.clone(), json!({"state": "active"}))],
    );
}

/// A watched path that does not exist yet is watched through the nearest
/// directory above it that does, and its creation runs the Source.
#[test]
fn an_absent_path_is_watched_through_its_parent_and_its_creation_runs_the_source() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let file = dir.join("later").join("state");
    let d = daemon(&watching(&file, &file));
    let sock = d.socket.path.clone();
    let get = json!({"op": "get", "key": "w.v"});
    assert_eq!(
        ask(&sock, get.clone())["data"],
        Value::Null,
        "nothing to read yet"
    );
    let path = file.to_string_lossy().into_owned();
    poll_until(
        "introspect watches",
        "the watch Active through the directory that exists",
        Duration::from_secs(10),
        Duration::from_millis(5),
        || watches(&sock),
        |w| *w == [(path.clone(), json!({"state": "active", "via": dir}))],
    );

    std::fs::create_dir(dir.join("later")).unwrap();
    poll_until(
        "introspect watches",
        "the watch moved onto the directory that was made",
        Duration::from_secs(10),
        Duration::from_millis(5),
        || watches(&sock),
        |w| {
            *w == [(
                path.clone(),
                json!({"state": "active", "via": dir.join("later")}),
            )]
        },
    );
    std::fs::write(&file, "1").unwrap();
    poll_until(
        "w.v",
        "the value the created file holds",
        Duration::from_secs(10),
        Duration::from_millis(5),
        || ask(&sock, get.clone())["data"].clone(),
        |v| *v == json!("1"),
    );
    poll_until(
        "introspect watches",
        "the watch on the file itself",
        Duration::from_secs(10),
        Duration::from_millis(5),
        || watches(&sock),
        |w| *w == [(path.clone(), json!({"state": "active"}))],
    );
}

/// A watch exists only while its instance is Active: once the Active window
/// (here one poll of one second) has passed, the instance decays and drops
/// the watch, so `introspect watches` reports none. A read after that is
/// checked by stat and answers the file as it is now, with no watch; and the
/// read reinstates the instance, which registers the watch again.
///
/// The window is real time (the lifecycle runs on the system clock, and a
/// poll is whole seconds), so this test takes one to two seconds.
#[test]
fn a_watch_goes_when_its_instance_decays_and_a_read_brings_it_back() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let file = dir.join("state");
    std::fs::write(&file, "1").unwrap();
    let d = daemon(&format!(
        "[daemon.providers.w.sources.s]\n\
         data = \"{{{{ '{file}' | read }}}}\"\n\
         result.v = \"{{{{ data }}}}\"\n\
         return = \"{{{{ result | watch('{file}') | poll('1s', 1) | budget('10s') }}}}\"\n",
        file = file.display()
    ));
    let sock = d.socket.path.clone();
    let get = json!({"op": "get", "key": "w.v"});
    assert_eq!(ask(&sock, get.clone())["data"], json!("1"));
    let path = file.to_string_lossy().into_owned();
    let active = || {
        poll_until(
            "introspect watches",
            "the watch Active",
            Duration::from_secs(10),
            Duration::from_millis(5),
            || watches(&sock),
            |w| *w == [(path.clone(), json!({"state": "active"}))],
        )
    };
    active();

    let since = std::time::Instant::now();
    poll_until(
        "introspect watches",
        "no watch once the instance has decayed",
        Duration::from_secs(10),
        Duration::from_millis(10),
        || watches(&sock),
        Vec::is_empty,
    );
    eprintln!(
        "introspect watches reached 0 {:?} after the watch was seen Active (Active window 1s)",
        since.elapsed()
    );

    std::fs::write(&file, "2").unwrap();
    assert_eq!(
        ask(&sock, get)["data"],
        json!("2"),
        "a read after decay is checked by stat: fresh with no watch"
    );
    active();
}
