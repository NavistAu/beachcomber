//! `defaults/direnv.toml`: the `state` Source, one instance per `.envrc`
//! (the nearest above the directory read), answering alone: whether direnv's
//! allow database allows it.

use super::{age, schedule, served};
use beachcomber::provider::{FailbackConfig, InvalidationStrategy};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The allow file direnv writes for the `.envrc` at `envrc`, under the allow
/// database `db`: named by the sha256 of its canonical path.
fn allow_file(db: &Path, envrc: &Path) -> PathBuf {
    let canon = envrc.canonicalize().unwrap();
    let hash = Sha256::digest(canon.to_string_lossy().as_bytes());
    let name: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    db.join(name)
}

/// `direnv allow` of the `.envrc` at `envrc`, in the allow database `db`.
fn allow(db: &Path, envrc: &Path) -> PathBuf {
    std::fs::create_dir_all(db).unwrap();
    let file = allow_file(db, envrc);
    std::fs::write(
        &file,
        envrc.canonicalize().unwrap().to_string_lossy().as_bytes(),
    )
    .unwrap();
    file
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

/// A symlinked `.envrc` is allowed by its canonical path, as direnv allows it.
#[test]
fn direnv_a_symlinked_envrc_is_hashed_by_its_canonical_path() {
    let data = tempfile::tempdir().unwrap();
    let s = served(&[("XDG_DATA_HOME", data.path().to_str().unwrap())], "");
    let real = s.home.join("shared/envrc");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::write(&real, "export FOO=bar\n").unwrap();
    std::os::unix::fs::symlink(&real, s.project.join(".envrc")).unwrap();
    allow(&data.path().join("direnv/allow"), &real);
    assert_eq!(
        s.get("direnv.status", Some(&s.project)),
        Some(json!("allowed"))
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
