//! `comb migrate` against this binary's schema. The migrations themselves are
//! exercised by the unit tests (`config::migrate`), with a test migration.

use std::process::Command;

/// `comb migrate args` with a temp config and state home: exit code, stdout,
/// stderr.
fn migrate(config: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let state = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_comb"))
        .arg("migrate")
        .args(args)
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_STATE_HOME", state.path())
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn a_config_at_the_current_schema_is_nothing_to_do() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("beachcomber");
    std::fs::create_dir(&dir).unwrap();
    let text = "# mine\n[daemon]\nlog_level = \"debug\"\n";
    std::fs::write(dir.join("config.toml"), text).unwrap();
    let (code, out, err) = migrate(home.path(), &[]);
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    assert_eq!(out, "nothing to migrate: config is at schema 1\n");
    assert_eq!(
        std::fs::read_to_string(dir.join("config.toml")).unwrap(),
        text
    );
}

#[test]
fn a_file_from_a_newer_comb_is_an_error() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("new.toml");
    std::fs::write(&file, "schema.version = 99\n").unwrap();
    let (code, _, err) = migrate(home.path(), &[file.to_str().unwrap()]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("newer comb"), "{err}");
}
