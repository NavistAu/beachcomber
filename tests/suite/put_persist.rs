//! `comb put --persist`: puts kept across a daemon restart in
//! `$XDG_CACHE_HOME/beachcomber/puts.json`, best effort.

use crate::common::daemon::{TestDaemon, isolate};
use crate::common::fetch::fetch;
use crate::common::socket::IsolatedSocket;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A `$HOME` and `$XDG_CONFIG_HOME` that outlive each daemon started on them,
/// so a second daemon finds what the first persisted.
struct Home {
    _base: tempfile::TempDir,
    home: PathBuf,
    xdg: PathBuf,
}

impl Home {
    fn new() -> Self {
        let base = tempfile::TempDir::new().unwrap();
        let root = base.path().canonicalize().unwrap();
        Self {
            home: root.join("home"),
            xdg: root.join("xdg"),
            _base: base,
        }
    }

    fn daemon(&self) -> TestDaemon {
        TestDaemon::serve_in_process(IsolatedSocket::new(), self.home.clone(), self.xdg.clone())
    }

    fn file(&self) -> PathBuf {
        self.home.join(".cache/beachcomber/puts.json")
    }

    fn entries(&self) -> Vec<Value> {
        serde_json::from_slice(&std::fs::read(self.file()).unwrap()).unwrap()
    }
}

/// `comb <args>` against `d`, which must succeed.
fn comb(d: &TestDaemon, home: &Path, args: &[&str]) {
    d.comb(home).args(args).assert().success();
}

fn get(d: &TestDaemon, key: &str, path: Option<&str>) -> Option<Value> {
    fetch(&d.socket.path, key, path)
        .unwrap()
        .and_then(|g| g.data)
}

#[test]
fn a_persisted_put_survives_a_restart_and_a_plain_one_does_not() {
    let h = Home::new();
    let dir = h.home.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.to_str().unwrap();
    {
        let d = h.daemon();
        comb(
            &d,
            &h.home,
            &["put", "--persist", "note", r#"{"msg": "hi"}"#],
        );
        comb(
            &d,
            &h.home,
            &["put", "--persist", "note.here", r#""proj""#, dir],
        );
        comb(&d, &h.home, &["put", "gone", r#"{"x": 1}"#]);
    }
    let d = h.daemon();
    assert_eq!(get(&d, "note.msg", None), Some(json!("hi")));
    assert_eq!(get(&d, "note.here", Some(dir)), Some(json!("proj")));
    assert_eq!(get(&d, "note.here", None), None);
    assert_eq!(get(&d, "gone.x", None), None);
    // Replayed as the put it was, so it is still marked.
    assert_eq!(get(&d, "note:cache.msg.persist", None), Some(json!(true)));
}

#[test]
fn a_persisted_ttl_put_comes_back_with_its_remaining_time_unless_it_expired() {
    let h = Home::new();
    {
        let d = h.daemon();
        comb(
            &d,
            &h.home,
            &["put", "--persist", "--ttl", "600s", "t.live", "1"],
        );
        comb(
            &d,
            &h.home,
            &["put", "--persist", "--ttl", "600s", "t.dead", "2"],
        );
    }
    // The daemon is down: one entry's expiry passes, the other's is 100s away.
    let now = beachcomber::persist::now_ms();
    let entries: Vec<Value> = h
        .entries()
        .into_iter()
        .map(|mut e| {
            let left: i64 = if e["key"] == "t.dead" {
                -1_000
            } else {
                100_000
            };
            e["expires"] = json!(now as i64 + left);
            e
        })
        .collect();
    std::fs::write(h.file(), serde_json::to_vec(&entries).unwrap()).unwrap();

    let d = h.daemon();
    assert_eq!(get(&d, "t.dead", None), None);
    assert_eq!(get(&d, "t.live", None), Some(json!(1)));
    assert_eq!(
        get(&d, "t:cache.live.poll.interval", None),
        Some(json!("100s"))
    );
}

#[test]
fn the_latest_put_to_an_address_decides() {
    let h = Home::new();
    {
        let d = h.daemon();
        comb(&d, &h.home, &["put", "--persist", "a.x", "1"]);
        comb(&d, &h.home, &["put", "--persist", "a.y", "2"]);
        comb(
            &d,
            &h.home,
            &["put", "--persist", "--ttl", "5s", "a.z", "3"],
        );
        assert_eq!(h.entries().len(), 3);
        comb(&d, &h.home, &["put", "a.x", "10"]);
        comb(&d, &h.home, &["put", "a.y", "null"]);
        assert_eq!(get(&d, "a:cache.x.persist", None), None);
        // The ttl runs out (valid 5s, stale 5s more), and its entry goes.
        crate::common::poll::poll_until(
            "puts.json",
            "no entries",
            Duration::from_secs(5),
            Duration::from_millis(5),
            || {
                d.advance(Duration::from_secs(6));
                h.entries()
            },
            |e| e.is_empty(),
        );
    }
    let d = h.daemon();
    assert_eq!(get(&d, "a", None), None);
}

#[test]
fn a_corrupt_file_is_warned_about_ignored_and_replaced() {
    let h = Home::new();
    std::fs::create_dir_all(h.file().parent().unwrap()).unwrap();
    std::fs::write(h.file(), b"{not json").unwrap();
    let socket = IsolatedSocket::new();
    let log = socket.dir.path().join("daemon.log");
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
    isolate(&mut cmd, &h.home, &h.xdg, &socket.path)
        .env("RUST_LOG", "warn")
        .args(["daemon", "--exit-with-parent", "--socket"])
        .arg(&socket.path);
    let mut child =
        libbeachcomber::spawn::launch(&mut cmd, Some(&log), libbeachcomber::spawn::READY_TIMEOUT)
            .expect("the daemon starts");
    let put = isolate(
        &mut std::process::Command::new(env!("CARGO_BIN_EXE_comb")),
        &h.home,
        &h.xdg,
        &socket.path,
    )
    .current_dir(&h.home)
    .args(["put", "--persist", "p.v", "1"])
    .status()
    .unwrap();
    crate::common::daemon::stop(&mut child);
    assert!(put.success());
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(
        text.contains("persisted puts") && text.contains("ignored"),
        "{text}"
    );
    assert_eq!(h.entries(), vec![json!({"key": "p.v", "data": 1})]);
}

#[test]
fn cache_reports_persist_on_a_persisted_node_and_refuses_a_put_to_it() {
    let h = Home::new();
    let d = h.daemon();
    comb(&d, &h.home, &["put", "--persist", "ns.a", "1"]);
    comb(&d, &h.home, &["put", "ns.b", "2"]);
    comb(&d, &h.home, &["put", "--persist", "root", r#"{"k": 1}"#]);
    assert_eq!(get(&d, "ns:cache.a.persist", None), Some(json!(true)));
    assert_eq!(get(&d, "ns:cache.b.persist", None), None);
    assert_eq!(get(&d, "root:cache.k.persist", None), Some(json!(true)));
    let out = d
        .comb(&h.home)
        .args(["put", "ns:cache.a.persist", "false"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(stderr.contains("is written by the daemon"), "{stderr}");
}
