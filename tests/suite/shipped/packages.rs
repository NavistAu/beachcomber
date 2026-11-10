//! `defaults/{brew,apt,dnf,pacman,apk}.toml`: outdated packages, from stand-in
//! package managers on a temp `PATH` that print recorded output (apt, apk,
//! dnf and pacman are Linux tools and are not run for real). The lists'
//! files (`checked`) are absolute system paths, so a test replaces the shipped
//! line's path with a temp directory, as `sudo.rs` does.
//!
//! The opt-in refreshes (the `update` Sources: `brew.update`, ...) ship off; a test turns
//! one on in a later file and sees the stand-in run.

use super::{age, served};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// A bin directory holding `(name, script body)` stand-ins, and `PATH` set to
/// it (this process's, which the Sources' commands inherit) and the system's.
struct Bin {
    _dir: TempDir,
    root: PathBuf,
    path: String,
}

fn bin(tools: &[(&str, &str)]) -> Bin {
    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let bindir = root.join("bin");
    std::fs::create_dir_all(&bindir).unwrap();
    for (name, body) in tools {
        crate::common::daemon::tool(&bindir, name, &format!("#!/bin/sh\n{body}\n"));
    }
    let path = format!(
        "{}:{}",
        bindir.display(),
        crate::common::daemon::system_bin().display()
    );
    // SAFETY: nextest runs each test in its own process, before its server.
    unsafe { std::env::set_var("PATH", &path) };
    Bin {
        _dir: dir,
        root,
        path,
    }
}

/// A stand-in that logs each call to `<root>/log`, prints `out` and exits `code`.
fn says(out: &str, code: i32) -> String {
    format!("echo \"$0 $*\" >> \"$(dirname \"$0\")/../log\"\ncat <<'EOT'\n{out}\nEOT\nexit {code}")
}

impl Bin {
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.join("log"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.rsplit('/').next().unwrap().to_string())
            .collect()
    }
}

/// The shipped line `path` (`["apt", "upgradable", "result", "checked"]`).
fn shipped(provider: &str, source: &str, field: &[&str]) -> String {
    let file = format!("{provider}.toml");
    let (_, body) = beachcomber::daemon::defaults::SHIPPED
        .iter()
        .find(|(name, _)| *name == file)
        .unwrap();
    let mut v: &toml::Value = &toml::from_str::<toml::Value>(body).unwrap();
    let v = {
        for k in ["daemon", "providers", provider, "sources", source]
            .iter()
            .chain(field)
        {
            v = &v[*k];
        }
        v
    };
    v.as_str().unwrap().to_string()
}

/// A layer setting `field` of the Source to the shipped line with `from`
/// replaced by `to`.
fn over(provider: &str, source: &str, field: &str, from: &str, to: &Path) -> String {
    let parts: Vec<&str> = field.split('.').collect();
    let line = shipped(provider, source, &parts);
    assert!(line.contains(from), "{line}");
    let line = line.replace(from, &to.display().to_string());
    assert!(!line.contains("'''"));
    format!("[daemon.providers.{provider}.sources.{source}]\n{field} = '''{line}'''\n")
}

fn mtime(path: &Path) -> i64 {
    use std::time::UNIX_EPOCH;
    std::fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn read(s: &super::Served, key: &str) -> Option<Value> {
    s.get(key, None)
}

const ON: &str = "[daemon.providers.NAME.sources.update]\nenabled = true\n";

// ---- brew ----

const BREW: &str = r#"case "$1" in
  --repository) echo "$(dirname "$0")/../repo" ;;
  outdated) [ "$HOMEBREW_NO_AUTO_UPDATE" = 1 ] || exit 3; echo "brew $*" >> "$(dirname "$0")/../log"; printf 'openssl@3\nnode\n' ;;
  update) echo "brew $*" >> "$(dirname "$0")/../log" ;;
esac"#;

fn brew_bin(script: &str) -> Bin {
    let b = bin(&[("brew", script)]);
    std::fs::create_dir_all(b.root.join("repo/.git")).unwrap();
    std::fs::write(b.root.join("repo/.git/FETCH_HEAD"), "").unwrap();
    age(&b.root.join("repo/.git/FETCH_HEAD"));
    b
}

/// Names, count and the age of FETCH_HEAD; brew is told not to update itself.
#[test]
fn brew_lists_the_outdated_and_never_auto_updates() {
    let b = brew_bin(BREW);
    let s = served(&[("PATH", &b.path)], "");
    assert_eq!(
        read(&s, "brew"),
        Some(json!({
            "packages": ["openssl@3", "node"],
            "count": 2,
            "checked": mtime(&b.root.join("repo/.git/FETCH_HEAD")),
        }))
    );
    assert_eq!(b.calls(), ["brew outdated --quiet"]);
}

/// Nothing outdated is a count of 0, an answer.
#[test]
fn brew_with_nothing_outdated_is_zero() {
    let b = brew_bin("case \"$1\" in --repository) echo \"$(dirname \"$0\")/../repo\" ;; esac");
    let s = served(&[("PATH", &b.path)], "");
    assert_eq!(read(&s, "brew.count"), Some(json!(0)));
    assert_eq!(read(&s, "brew.packages"), Some(json!([])));
}

/// A brew that fails is no answer, not a count of 0.
#[test]
fn brew_failing_is_absent() {
    let b = brew_bin("exit 1");
    let s = served(&[("PATH", &b.path)], "");
    assert_eq!(read(&s, "brew"), None);
}

/// No FETCH_HEAD yet: no `checked`, the rest answers.
#[test]
fn brew_without_fetch_head_has_no_checked() {
    let b = brew_bin(BREW);
    std::fs::remove_file(b.root.join("repo/.git/FETCH_HEAD")).unwrap();
    let s = served(&[("PATH", &b.path)], "");
    assert_eq!(read(&s, "brew.count"), Some(json!(2)));
    assert_eq!(read(&s, "brew.checked"), None);
}

/// Without `brew` on the PATH the provider is off, and so is its refresh.
#[test]
fn brew_missing_is_off() {
    let b = bin(&[]);
    let s = served(&[("PATH", &b.path)], &ON.replace("NAME", "brew"));
    assert_eq!(read(&s, "brew"), None);
    assert_eq!(read(&s, "brew.at"), None);
}

/// The refresh ships off, runs nothing; turned on it runs `brew update`.
#[test]
fn brew_update_is_opt_in() {
    let b = brew_bin(BREW);
    let s = served(&[("PATH", &b.path)], "");
    assert_eq!(read(&s, "brew.at"), None);
    assert!(b.calls().is_empty(), "{:?}", b.calls());
    assert_eq!(
        read(&s, "brew.count"),
        Some(json!(2)),
        "its sibling answers"
    );
    assert_eq!(b.calls(), ["brew outdated --quiet"]);

    let b = brew_bin(BREW);
    let s = served(&[("PATH", &b.path)], &ON.replace("NAME", "brew"));
    let at = read(&s, "brew.at").expect("ran");
    assert!(at.is_number(), "{at}");
    assert_eq!(b.calls(), ["brew update --quiet"]);
}

// ---- apt ----

const APT: &str = "WARNING: apt does not have a stable CLI interface. Use with caution in scripts.

Listing...
openssl/jammy-updates,jammy-security 3.0.2-0ubuntu1.15 amd64 [upgradable from: 3.0.2-0ubuntu1.14]
curl/jammy-updates 7.81.0-1ubuntu1.18 amd64 [upgradable from: 7.81.0-1ubuntu1.17]";

#[test]
fn apt_lists_the_upgradable_and_dates_the_lists() {
    let b = bin(&[("apt", &says(APT, 0))]);
    let lists = b.root.join("lists");
    std::fs::create_dir_all(&lists).unwrap();
    let pkgs = lists.join("archive_ubuntu_dists_jammy_main_binary-amd64_Packages.lz4");
    let release = lists.join("archive_ubuntu_dists_jammy_InRelease");
    for f in [&pkgs, &release, &lists.join("lock")] {
        std::fs::write(f, "").unwrap();
    }
    age(&pkgs);
    // The lock file apt touches on every run is not a refresh; the newer file counts.
    std::fs::File::open(&release)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(5))
        .unwrap();
    let conf = over(
        "apt",
        "upgradable",
        "result.checked",
        "/var/lib/apt/lists",
        &lists,
    );
    let s = served(&[("PATH", &b.path)], &conf);
    assert_eq!(
        read(&s, "apt"),
        Some(json!({
            "packages": ["openssl", "curl"],
            "count": 2,
            "checked": mtime(&release),
        }))
    );
    assert_eq!(b.calls(), ["apt list --upgradable"]);
}

#[test]
fn apt_with_nothing_upgradable_is_zero() {
    let b = bin(&[("apt", &says("Listing...", 0))]);
    let s = served(&[("PATH", &b.path)], "");
    assert_eq!(read(&s, "apt.count"), Some(json!(0)));
}

#[test]
fn apt_failing_or_missing_is_absent() {
    let b = bin(&[("apt", &says("", 100))]);
    assert_eq!(read(&served(&[("PATH", &b.path)], ""), "apt"), None);
    let b = bin(&[]);
    assert_eq!(read(&served(&[("PATH", &b.path)], ""), "apt"), None);
}

// ---- apk ----

const APK: &str = "Installed:                                Available:
busybox-1.36.1-r15                                  < 1.36.1-r16
libstdc++-13.2.1_git20231014-r0                     < 13.2.1_git20231014-r1
py3-pip-23.0-r0                                     < 23.1-r0";

#[test]
fn apk_lists_the_upgradable_and_dates_the_index() {
    let b = bin(&[("apk", &says(APK, 0))]);
    let cache = b.root.join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let index = cache.join("APKINDEX.abc123.tar.gz");
    std::fs::write(&index, "").unwrap();
    age(&index);
    let conf = over(
        "apk",
        "upgradable",
        "result.checked",
        "/var/cache/apk",
        &cache,
    );
    let s = served(&[("PATH", &b.path)], &conf);
    assert_eq!(
        read(&s, "apk"),
        Some(json!({
            "packages": ["busybox", "libstdc++", "py3-pip"],
            "count": 3,
            "checked": mtime(&index),
        }))
    );
    assert_eq!(b.calls(), ["apk version -l <"]);
}

#[test]
fn apk_up_to_date_failing_or_missing() {
    let b = bin(&[("apk", &says("", 0))]);
    assert_eq!(
        read(&served(&[("PATH", &b.path)], ""), "apk.count"),
        Some(json!(0))
    );
    let b = bin(&[("apk", &says("", 1))]);
    assert_eq!(read(&served(&[("PATH", &b.path)], ""), "apk"), None);
    let b = bin(&[]);
    assert_eq!(read(&served(&[("PATH", &b.path)], ""), "apk"), None);
}

// ---- dnf ----

const DNF: &str = "Last metadata expiration check: 0:14:01 ago on Tue 10 Nov 2026.

kernel.x86_64                     6.5.6-300.fc39                 updates
openssl-libs.x86_64               1:3.1.1-4.fc39                 updates
tzdata.noarch                     2023d-1.fc39                   updates

Obsoleting Packages
foo.x86_64                        1.0-1                          updates
    bar.x86_64                    0.9-1                          @system";

fn dnf_conf(root: &Path) -> String {
    over(
        "dnf",
        "updates",
        "data",
        "/var/cache/dnf/*/repodata/repomd.xml",
        &root.join("*/repodata/repomd.xml"),
    )
}

/// Exit 100 is "updates"; the obsoleting section and the header are not packages.
#[test]
fn dnf_exit_100_lists_the_updates_and_dates_the_metadata() {
    let b = bin(&[("dnf", &says(DNF, 100))]);
    let cache = b.root.join("dnf");
    std::fs::create_dir_all(cache.join("updates-abc/repodata")).unwrap();
    let repomd = cache.join("updates-abc/repodata/repomd.xml");
    std::fs::write(&repomd, "").unwrap();
    age(&repomd);
    let s = served(&[("PATH", &b.path), ("USER", "tester")], &dnf_conf(&cache));
    assert_eq!(
        read(&s, "dnf"),
        Some(json!({
            "packages": ["kernel", "openssl-libs", "tzdata"],
            "count": 3,
            "checked": mtime(&repomd),
        }))
    );
    assert_eq!(b.calls(), ["dnf check-update --cacheonly"]);
}

#[test]
fn dnf_exit_0_is_none_and_exit_1_is_a_failure() {
    let b = bin(&[(
        "dnf",
        &says("Last metadata expiration check: 0:01:00 ago.", 0),
    )]);
    let s = served(&[("PATH", &b.path), ("USER", "tester")], "");
    assert_eq!(read(&s, "dnf.count"), Some(json!(0)));
    let b = bin(&[(
        "dnf",
        &says("Error: Cache-only enabled but no cache for 'updates'", 1),
    )]);
    let s = served(&[("PATH", &b.path), ("USER", "tester")], "");
    assert_eq!(read(&s, "dnf"), None);
    let b = bin(&[]);
    let s = served(&[("PATH", &b.path), ("USER", "tester")], "");
    assert_eq!(read(&s, "dnf"), None);
}

#[test]
fn dnf_update_is_opt_in() {
    let b = bin(&[("dnf", &says("", 0))]);
    let s = served(&[("PATH", &b.path), ("USER", "tester")], "");
    assert_eq!(read(&s, "dnf.at"), None);
    let b = bin(&[("dnf", &says("", 0))]);
    let s = served(
        &[("PATH", &b.path), ("USER", "tester")],
        &ON.replace("NAME", "dnf"),
    );
    assert!(read(&s, "dnf.at").is_some());
    assert!(
        b.calls().contains(&"dnf makecache --refresh".to_string()),
        "{:?}",
        b.calls()
    );
}

// ---- pacman ----

const PACMAN: &str = "linux 6.5.9.arch2-1 -> 6.5.10.arch1-1
openssl 3.1.4-1 -> 3.1.4-2";

#[test]
fn pacman_exit_0_lists_the_updates_and_dates_the_copy() {
    let b = bin(&[("checkupdates", &says(PACMAN, 0))]);
    let db = b.root.join("checkup-db");
    std::fs::create_dir_all(db.join("sync")).unwrap();
    let core = db.join("sync/core.db");
    std::fs::write(&core, "").unwrap();
    age(&core);
    let s = served(
        &[("PATH", &b.path), ("CHECKUPDATES_DB", db.to_str().unwrap())],
        "",
    );
    assert_eq!(
        read(&s, "pacman"),
        Some(json!({
            "packages": ["linux", "openssl"],
            "count": 2,
            "checked": mtime(&core),
        }))
    );
    assert_eq!(b.calls(), ["checkupdates --nosync --nocolor"]);
}

/// Exit 2 is "no updates"; exit 1 is a failure.
#[test]
fn pacman_exit_2_is_none_and_exit_1_is_a_failure() {
    let b = bin(&[("checkupdates", &says("", 2))]);
    let s = served(&[("PATH", &b.path)], "");
    assert_eq!(read(&s, "pacman.count"), Some(json!(0)));
    let b = bin(&[("checkupdates", &says("", 1))]);
    let s = served(&[("PATH", &b.path)], "");
    assert_eq!(read(&s, "pacman"), None);
    let b = bin(&[]);
    assert_eq!(read(&served(&[("PATH", &b.path)], ""), "pacman"), None);
}

/// The opt-in refresh is a plain `checkupdates`, which syncs its copy.
#[test]
fn pacman_update_is_opt_in() {
    let b = bin(&[("checkupdates", &says(PACMAN, 0))]);
    let s = served(&[("PATH", &b.path)], "");
    assert_eq!(read(&s, "pacman.at"), None);
    let b = bin(&[("checkupdates", &says(PACMAN, 0))]);
    let s = served(&[("PATH", &b.path)], &ON.replace("NAME", "pacman"));
    assert!(read(&s, "pacman.at").is_some());
    assert!(
        b.calls().contains(&"checkupdates --nocolor".to_string()),
        "{:?}",
        b.calls()
    );
}

/// `mise`'s gate: off without the tool (the shipped stub bin is not on this PATH).
#[test]
fn mise_is_off_without_mise() {
    let b = bin(&[]);
    let s = served(&[("PATH", &b.path)], "");
    assert_eq!(read(&s, "mise"), None);
    let _ = s.project;
}
