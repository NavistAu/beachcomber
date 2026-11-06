//! `defaults/sudo.toml`: the `state` Source, one instance at `~`, whether the
//! user's sudo timestamp is fresh.
//!
//! The timestamp directories are absolute paths only root writes, so a test
//! stands two temp directories in for them by setting the Source's `data`
//! line in a later file: the shipped line, with its two directories
//! replaced. That a later file takes any line of a shipped Source is itself
//! part of what is tested.

use super::{age, schedule, served};
use beachcomber::provider::{FailbackConfig, InvalidationStrategy};
use serde_json::json;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// The user the tests' Sources run as.
const USER: &str = "tester";

/// The shipped `data` line.
fn shipped_data() -> String {
    let (_, body) = beachcomber::daemon::defaults::SHIPPED
        .iter()
        .find(|(name, _)| *name == "sudo.toml")
        .expect("sudo.toml is shipped");
    let parsed: toml::Value = toml::from_str(body).unwrap();
    parsed["daemon"]["providers"]["sudo"]["sources"]["state"]["data"]
        .as_str()
        .unwrap()
        .to_string()
}

/// A layer setting the Source's `data` to the shipped line over `first` and
/// `second` in place of `/var/run/sudo/ts` and `/run/sudo/ts`.
fn over(first: &Path, second: &Path) -> String {
    let data = shipped_data();
    let (a, b) = ("'/var/run/sudo/ts'", "'/run/sudo/ts'");
    assert!(data.contains(a) && data.contains(b), "{data}");
    let data = data
        .replace(a, &format!("'{}'", first.display()))
        .replace(b, &format!("'{}'", second.display()));
    assert!(!data.contains("'''"), "{data}");
    format!("[daemon.providers.sudo.sources.state]\ndata = '''{data}'''\n")
}

/// Two stand-in timestamp directories.
fn dirs() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (first, second) = (root.join("var-run-ts"), root.join("run-ts"));
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    (tmp, first, second)
}

/// `dir/<USER>`, a timestamp file `ago` old.
fn timestamp(dir: &Path, ago: Duration) -> std::path::PathBuf {
    let file = dir.join(USER);
    std::fs::write(&file, b"").unwrap();
    std::fs::File::open(&file)
        .unwrap()
        .set_modified(SystemTime::now() - ago)
        .unwrap();
    file
}

/// A timestamp file touched a moment ago is `active`.
#[test]
fn sudo_readable_fresh_timestamp_is_true() {
    let (_tmp, first, second) = dirs();
    timestamp(&first, Duration::ZERO);
    let s = served(&[("USER", USER)], &over(&first, &second));
    assert_eq!(s.get("sudo", None), Some(json!({"active": true})));
}

/// One ten minutes old is past sudo's five: `active` is false, an answer.
#[test]
fn sudo_readable_expired_timestamp_is_false() {
    let (_tmp, first, second) = dirs();
    timestamp(&first, Duration::from_secs(600));
    let s = served(&[("USER", USER)], &over(&first, &second));
    assert_eq!(s.get("sudo.active", None), Some(json!(false)));
}

/// One ahead of the clock is not fresh.
#[test]
fn sudo_a_timestamp_from_the_future_is_false() {
    let (_tmp, first, second) = dirs();
    let file = timestamp(&first, Duration::ZERO);
    std::fs::File::open(&file)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(120))
        .unwrap();
    let s = served(&[("USER", USER)], &over(&first, &second));
    assert_eq!(s.get("sudo.active", None), Some(json!(false)));
}

/// No timestamp file, and no `$USER`, are nothing to say: absent.
#[test]
fn sudo_unreadable_path_produces_no_active_field() {
    let (_tmp, first, second) = dirs();
    let s = served(&[("USER", USER)], &over(&first, &second));
    assert_eq!(s.get("sudo", None), None);
    timestamp(&first, Duration::ZERO);
    let s = served(&[], &over(&first, &second));
    assert_eq!(s.get("sudo", None), None, "no $USER");
}

/// The second directory answers when the first has no regular file for
/// the user.
#[test]
fn sudo_the_second_directory_when_the_first_has_no_file() {
    let (_tmp, first, second) = dirs();
    std::fs::create_dir_all(first.join(USER)).unwrap();
    timestamp(&second, Duration::ZERO);
    let s = served(&[("USER", USER)], &over(&first, &second));
    assert_eq!(s.get("sudo.active", None), Some(json!(true)));
}

/// A timestamp directory the daemon cannot search, as root's is on most
/// Linux systems, is passed over without an error: the next one answers, or
/// nothing does.
#[test]
fn sudo_a_directory_that_cannot_be_searched_is_passed_over() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root searches any directory");
        return;
    }
    let (_tmp, first, second) = dirs();
    timestamp(&first, Duration::from_secs(600));
    std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o000)).unwrap();
    let s = served(&[("USER", USER)], &over(&first, &second));
    let answer = s.ask(&json!({"op": "get", "key": "sudo", "wait": "forever"}));
    assert_eq!(answer["ok"], json!(true), "{answer}");
    assert!(
        answer.get("data").is_none(),
        "absent, not an error: {answer}"
    );
    timestamp(&second, Duration::ZERO);
    let s = served(&[("USER", USER)], &over(&first, &second));
    assert_eq!(s.get("sudo.active", None), Some(json!(true)));
    std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// With nothing watching and nothing polling, a `sudo -v` (the timestamp
/// touched) is seen by the next read: the file the lines read is the
/// record's input.
#[test]
fn sudo_a_touched_timestamp_is_seen_by_the_next_read() {
    let (_tmp, first, second) = dirs();
    let file = timestamp(&first, Duration::from_secs(600));
    age(&first);
    let s = served(&[("USER", USER)], &over(&first, &second));
    assert_eq!(s.get("sudo.active", None), Some(json!(false)));
    let ran = s.ran_at("sudo", None, "state");
    for _ in 0..5 {
        s.get("sudo.active", None);
    }
    assert_eq!(s.ran_at("sudo", None, "state"), ran);
    std::fs::File::open(&file)
        .unwrap()
        .set_modified(SystemTime::now())
        .unwrap();
    assert_eq!(s.get("sudo.active", None), Some(json!(true)));
}

/// The shipped paths, read as the daemon reads them: on macOS sudo keeps no
/// timestamp there, so a user's daemon has nothing to say, as the Rust
/// provider had none (its `/var/db/sudo/<user>` is root's alone).
#[cfg(target_os = "macos")]
#[test]
fn sudo_on_macos_is_absent_for_a_users_daemon() {
    let user = std::env::var("USER").unwrap_or_else(|_| USER.into());
    let s = served(&[("USER", &user)], "");
    let answer = s.ask(&json!({"op": "get", "key": "sudo", "wait": "forever"}));
    assert_eq!(answer["ok"], json!(true), "{answer}");
    assert!(answer.get("data").is_none(), "{answer}");
}

/// The Rust provider's schedule, kept: one instance, polled every 30 s
/// twice, a failure retried every 30 s three times, its field `active`.
#[test]
fn sudo_provider_metadata() {
    let (meta, rooted) = schedule("sudo", "state");
    assert!(!rooted);
    let fields: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(fields, ["active"]);
    assert_eq!(
        meta.invalidation,
        InvalidationStrategy::Poll {
            interval_secs: 30,
            count: 2,
        }
    );
    assert_eq!(
        meta.failback,
        FailbackConfig {
            reattempts: 3,
            interval_secs: 30
        }
    );
}
