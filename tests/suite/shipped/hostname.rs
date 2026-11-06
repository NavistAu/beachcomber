//! `defaults/hostname.toml`: the machine's name, from `sys`.

use super::{scenario, scenario_with};
use serde_json::json;

/// What `gethostname(3)` says, as the test sees it.
fn this_host() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is writable for its length.
    assert_eq!(
        unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) },
        0
    );
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

#[test]
fn hostname_provider_executes() {
    let s = scenario();
    let host = s.get("hostname", &s.project).expect("hostname answers");
    assert_eq!(host["name"], json!(this_host()), "{host}");
    assert!(!this_host().is_empty());
}

#[test]
fn hostname_provider_short_is_prefix() {
    let s = scenario();
    let host = s.get("hostname", &s.project).expect("hostname answers");
    let name = host["name"].as_str().expect("a string name");
    assert_eq!(
        host["short"],
        json!(name.split('.').next().unwrap()),
        "{host}"
    );
}

/// One instance, at `~`: the same answer from any directory.
#[test]
fn hostname_provider_ignores_path() {
    let s = scenario();
    assert_eq!(
        s.get("hostname.name", &s.project),
        s.get("hostname.name", &s.daemon.home)
    );
}

/// The deliberate difference: polled every 10 minutes while read, where the
/// Rust Source ran once and never again until evicted, so a renamed machine
/// went unseen for the daemon's life.
#[test]
fn hostname_is_polled_every_ten_minutes() {
    let s = scenario();
    // A cache leaf describes a record: the read makes it.
    assert!(s.get("hostname.name", &s.project).is_some());
    let leaf = s
        .get("hostname:cache.name", &s.project)
        .expect("a cache leaf");
    assert_eq!(
        leaf["poll"],
        json!({"interval": "600s", "count": 6}),
        "{leaf}"
    );
}

#[test]
fn the_shipped_file_sets_the_hostname_source() {
    let s = scenario();
    let rows = s.origins("daemon.providers.hostname");
    assert!(!rows.is_empty());
    for row in &rows {
        let file = row["file"].as_str().unwrap_or_default();
        assert!(file.ends_with("/defaults/hostname.toml"), "{row}");
    }
}

/// What this phase is for: a user sets one line of a shipped Source and
/// keeps the rest.
#[test]
fn a_conf_d_line_overrides_one_leaf_of_the_shipped_source() {
    let s = scenario_with(&[(
        "conf.d/10-host.toml",
        "[daemon.providers.hostname.sources.host]\nresult.name = \"{{ data.name | upper }}\"\n",
    )]);
    let host = s.get("hostname", &s.project).expect("hostname answers");
    assert_eq!(host["name"], json!(this_host().to_uppercase()), "{host}");
    assert_eq!(
        host["short"],
        json!(this_host().split('.').next().unwrap()),
        "the shipped short line is kept: {host}"
    );
}
