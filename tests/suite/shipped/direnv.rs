//! `defaults/direnv.toml`: the `state` Source, one instance per `.envrc`
//! (the nearest above the directory read), answering alone: whether direnv's
//! allow database allows it.

use super::{age, schedule, served};
use beachcomber::provider::{FailbackConfig, InvalidationStrategy};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The name direnv gives an allow file: the sha256 of the `.envrc`'s absolute
/// path (symlinks not resolved), a newline, and its contents.
fn allow_name(path: &str, contents: &str) -> String {
    let hash = Sha256::digest(format!("{path}\n{contents}").as_bytes());
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// The allow file direnv writes for the `.envrc` at `envrc`, under the allow
/// database `db`.
fn allow_file(db: &Path, envrc: &Path) -> PathBuf {
    let contents = std::fs::read_to_string(envrc).unwrap();
    db.join(allow_name(envrc.to_str().unwrap(), &contents))
}

/// `direnv allow` of the `.envrc` at `envrc`, in the allow database `db`.
fn allow(db: &Path, envrc: &Path) -> PathBuf {
    std::fs::create_dir_all(db).unwrap();
    let file = allow_file(db, envrc);
    std::fs::write(&file, envrc.to_str().unwrap().as_bytes()).unwrap();
    file
}

/// `direnv deny` of the `.envrc` at `envrc`, in the deny database `db`: named
/// by the sha256 of the path and a newline, whatever the contents.
fn deny(db: &Path, envrc: &Path) {
    std::fs::create_dir_all(db).unwrap();
    let hash = Sha256::digest(format!("{}\n", envrc.to_str().unwrap()).as_bytes());
    let name: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(db.join(name), envrc.to_str().unwrap().as_bytes()).unwrap();
}

/// The scheme itself, against a name direnv 2.38.2 wrote: in a scratch HOME
/// and `XDG_DATA_HOME`, `printf 'export FOO=bar\n' > golden/.envrc` and
/// `direnv allow` there made this file.
#[test]
fn direnv_allow_name_is_what_real_direnv_wrote() {
    assert_eq!(
        allow_name(
            "/private/tmp/claude-501/-Users-jhogendorn-ws-beachcomber/4b310f75-9d71-4175-abb3-1a9096f44efb/scratchpad/dx/golden/.envrc",
            "export FOO=bar\n"
        ),
        "cf08a079efdb564d51813f7e4e09286c3c470e9c3f610e20b62133c8fc71a7bc"
    );
}

/// An `.envrc` in `dir`.
fn envrc(dir: &Path) -> PathBuf {
    let file = dir.join(".envrc");
    std::fs::write(&file, "export FOO=bar\n").unwrap();
    file
}

/// An allow file for the `.envrc` is `allowed`.
#[test]
fn direnv_allowed_when_allow_db_entry_exists() {
    let data = tempfile::tempdir().unwrap();
    let data_home = data.path().to_str().unwrap();
    let s = served(&[("XDG_DATA_HOME", data_home)], "");
    let rc = envrc(&s.project);
    allow(&data.path().join("direnv/allow"), &rc);
    assert_eq!(
        s.get("direnv", Some(&s.project)),
        Some(json!({"status": "allowed", "allowed": true}))
    );
}

/// No allow file is `blocked`, `allowed` false: an answer, not an absence.
#[test]
fn direnv_blocked_when_no_allow_db_entry() {
    let data = tempfile::tempdir().unwrap();
    let s = served(&[("XDG_DATA_HOME", data.path().to_str().unwrap())], "");
    envrc(&s.project);
    assert_eq!(
        s.get("direnv", Some(&s.project)),
        Some(json!({"status": "blocked", "allowed": false}))
    );
}

/// Without `$XDG_DATA_HOME` the database is `~/.local/share/direnv/allow`.
#[test]
fn direnv_allow_db_defaults_under_home() {
    let s = served(&[], "");
    let rc = envrc(&s.project);
    allow(&s.home.join(".local/share/direnv/allow"), &rc);
    assert_eq!(s.get("direnv.allowed", Some(&s.project)), Some(json!(true)));
}

/// An empty `$XDG_DATA_HOME` is unset, as direnv reads it: the database is
/// the default one. (The Rust provider looked under a relative
/// `direnv/allow`, against the daemon's working directory.)
#[test]
fn direnv_an_empty_xdg_data_home_is_the_default() {
    let s = served(&[("XDG_DATA_HOME", "")], "");
    let rc = envrc(&s.project);
    allow(&s.home.join(".local/share/direnv/allow"), &rc);
    assert_eq!(s.get("direnv.allowed", Some(&s.project)), Some(json!(true)));
}

/// A symlinked `.envrc` is named by the link's path, as direnv names it (it
/// does not resolve the link), with the target's contents.
#[test]
fn direnv_a_symlinked_envrc_is_named_by_the_link() {
    let data = tempfile::tempdir().unwrap();
    let db = data.path().join("direnv/allow");
    let s = served(&[("XDG_DATA_HOME", data.path().to_str().unwrap())], "");
    let real = s.home.join("shared/envrc");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::write(&real, "export FOO=bar\n").unwrap();
    let link = s.project.join(".envrc");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    allow(&db, &real);
    assert_eq!(
        s.get("direnv.status", Some(&s.project)),
        Some(json!("blocked")),
        "the target's name is not the link's"
    );
    allow(&db, &link);
    assert_eq!(
        s.get("direnv.status", Some(&s.project)),
        Some(json!("allowed"))
    );
}

/// An `.envrc` edited after `direnv allow` is no longer allowed: the next read
/// reads the new contents, which name another file.
#[test]
fn direnv_an_edited_envrc_is_blocked_until_allowed_again() {
    let data = tempfile::tempdir().unwrap();
    let db = data.path().join("direnv/allow");
    let s = served(&[("XDG_DATA_HOME", data.path().to_str().unwrap())], "");
    let rc = envrc(&s.project);
    age(&rc);
    allow(&db, &rc);
    assert_eq!(
        s.get("direnv.status", Some(&s.project)),
        Some(json!("allowed"))
    );
    std::fs::write(&rc, "export FOO=baz\n").unwrap();
    assert_eq!(
        s.get("direnv.status", Some(&s.project)),
        Some(json!("blocked"))
    );
    allow(&db, &rc);
    assert_eq!(
        s.get("direnv.status", Some(&s.project)),
        Some(json!("allowed"))
    );
}

/// `direnv deny` wins over an allow file, and is `denied`, `allowed` false.
#[test]
fn direnv_a_denied_envrc_is_denied() {
    let data = tempfile::tempdir().unwrap();
    let s = served(&[("XDG_DATA_HOME", data.path().to_str().unwrap())], "");
    let rc = envrc(&s.project);
    allow(&data.path().join("direnv/allow"), &rc);
    deny(&data.path().join("direnv/deny"), &rc);
    assert_eq!(
        s.get("direnv", Some(&s.project)),
        Some(json!({"status": "denied", "allowed": false}))
    );
}

/// No `.envrc` is no instance: nothing at all, not `blocked`.
#[test]
fn direnv_empty_when_no_envrc() {
    let s = served(&[], "");
    assert_eq!(s.get("direnv", Some(&s.project)), None);
    assert_eq!(s.get("direnv", Some(&s.home)), None);
}

/// A directory below an `.envrc` reads its instance; an `.envrc` nested in
/// another answers alone.
#[test]
fn direnv_root_returns_envrc_dir_from_subdir() {
    let data = tempfile::tempdir().unwrap();
    let s = served(&[("XDG_DATA_HOME", data.path().to_str().unwrap())], "");
    let rc = envrc(&s.project);
    allow(&data.path().join("direnv/allow"), &rc);
    let sub = s.project.join("src/nested");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(s.get("direnv.status", Some(&sub)), Some(json!("allowed")));
    s.ran_at("direnv", Some(&s.project), "state");
    envrc(&sub);
    assert_eq!(
        s.get("direnv.status", Some(&sub)),
        Some(json!("blocked")),
        "the nearer .envrc answers, and it is not allowed"
    );
}

/// With nothing watching and nothing polling, `direnv allow` (the allow file
/// made) and `direnv deny` (removed) are each seen by the next read, though
/// the Source watches only the `.envrc`; and a read with nothing changed runs
/// nothing.
#[test]
fn direnv_allow_and_deny_are_seen_by_the_next_read() {
    let data = tempfile::tempdir().unwrap();
    let db = data.path().join("direnv/allow");
    std::fs::create_dir_all(&db).unwrap();
    let s = served(&[("XDG_DATA_HOME", data.path().to_str().unwrap())], "");
    let rc = envrc(&s.project);
    age(&rc);
    assert_eq!(
        s.get("direnv.allowed", Some(&s.project)),
        Some(json!(false))
    );
    let ran = s.ran_at("direnv", Some(&s.project), "state");
    for _ in 0..5 {
        s.get("direnv.allowed", Some(&s.project));
    }
    assert_eq!(s.ran_at("direnv", Some(&s.project), "state"), ran);

    let file = allow(&db, &rc);
    assert_eq!(
        s.get("direnv.allowed", Some(&s.project)),
        Some(json!(true)),
        "direnv allow"
    );
    std::fs::remove_file(&file).unwrap();
    assert_eq!(
        s.get("direnv.allowed", Some(&s.project)),
        Some(json!(false)),
        "direnv deny"
    );
}

/// The Rust provider's schedule, kept: watch the `.envrc`, retry a failure
/// every minute three times, the nearest root alone.
#[test]
fn direnv_keeps_its_schedule() {
    let (meta, rooted) = schedule("direnv", "state");
    assert!(rooted);
    assert_eq!(
        meta.invalidation,
        InvalidationStrategy::Watch {
            patterns: vec![".envrc".into()],
            abs_paths: vec![],
        }
    );
    assert_eq!(
        meta.failback,
        FailbackConfig {
            reattempts: 3,
            interval_secs: 60
        }
    );
    assert!(meta.nearest);
    assert_eq!(meta.decay, None, "the provider's decay prototype");
}
