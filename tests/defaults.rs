//! Managed defaults (spec §3.3): the shipped templates are files the daemon
//! writes to `<config-dir>/defaults/` on start and seeds as the lowest layer.
//!
//! The writer is exercised directly against a temp directory; the daemon-level
//! tests own their `$HOME` and `$XDG_CONFIG_HOME` (nextest runs each test in
//! its own process, the assumption `tests/seed.rs` makes) so nothing lands in
//! the developer's real `~/.config/beachcomber`.

mod common;

use beachcomber::daemon::defaults::{
    MARKER_PREFIX, Report, SHIPPED, is_current, write_managed_defaults,
};
use serde_json::json;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;

/// A version for the writer tests: what matters is that it is not what any
/// fixture below writes by hand.
const V: &str = "9.9.9";

fn marker(version: &str) -> String {
    format!("{MARKER_PREFIX}{version}")
}

fn names(report_field: &[String]) -> Vec<&str> {
    report_field.iter().map(String::as_str).collect()
}

fn shipped_names() -> Vec<&'static str> {
    SHIPPED.iter().map(|(n, _)| *n).collect()
}

fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

// ── the writer's state table ─────────────────────────────────────────────────

#[test]
fn an_absent_directory_gets_every_shipped_file() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("defaults");

    let report = write_managed_defaults(&dir, V).unwrap();

    assert_eq!(
        report,
        Report {
            dir: dir.clone(),
            written: shipped_names().iter().map(|s| s.to_string()).collect(),
            deleted: vec![],
            unmanaged: vec![],
            files: 8,
        }
    );
    for (name, body) in SHIPPED {
        let written = std::fs::read_to_string(dir.join(name)).unwrap();
        let (first, rest) = written.split_once('\n').unwrap();
        assert_eq!(first, marker(V), "{name}: the first line is the marker");
        assert_eq!(
            rest, *body,
            "{name}: the rest is the embedded file verbatim"
        );
        assert!(is_current(&written, V));
        assert!(!is_current(&written, "0.0.1"));
    }
}

#[test]
fn a_stale_marker_is_rewritten() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("python.toml"),
        "# Managed by comb 0.0.1\nx = 1\n",
    )
    .unwrap();

    let report = write_managed_defaults(tmp.path(), V).unwrap();

    assert!(names(&report.written).contains(&"python.toml"));
    let body = std::fs::read_to_string(tmp.path().join("python.toml")).unwrap();
    assert!(is_current(&body, V));
    assert!(
        body.contains("venv_name"),
        "the body is this version's: {body}"
    );
}

#[test]
fn a_shipped_name_without_a_marker_is_rewritten() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("python.toml"), "x = 1\n").unwrap();

    let report = write_managed_defaults(tmp.path(), V).unwrap();

    assert!(names(&report.written).contains(&"python.toml"));
    assert!(is_current(
        &std::fs::read_to_string(tmp.path().join("python.toml")).unwrap(),
        V
    ));
}

#[test]
fn a_current_file_is_left_alone_even_when_edited_below_the_marker() {
    let tmp = TempDir::new().unwrap();
    let edited = format!(
        "{}\n[providers.\"python:template\"]\nversion = \"mine\"\n",
        marker(V)
    );
    std::fs::write(tmp.path().join("python.toml"), &edited).unwrap();

    let report = write_managed_defaults(tmp.path(), V).unwrap();

    assert!(!names(&report.written).contains(&"python.toml"));
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("python.toml")).unwrap(),
        edited,
        "bytes unchanged"
    );
    assert_eq!(report.written.len(), 7, "the other seven are written");
}

#[test]
fn a_retired_managed_file_is_deleted() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("old.toml"),
        format!("{}\nx = 1\n", marker(V)),
    )
    .unwrap();

    let report = write_managed_defaults(tmp.path(), V).unwrap();

    assert_eq!(report.deleted, vec!["old.toml".to_string()]);
    assert!(!tmp.path().join("old.toml").exists());
    assert!(report.unmanaged.is_empty());
}

#[test]
fn an_unmanaged_toml_is_kept_and_listed_and_other_files_are_ignored() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("mine.toml"), "x = 1\n").unwrap();
    std::fs::write(tmp.path().join("notes.txt"), format!("{}\n", marker(V))).unwrap();

    let report = write_managed_defaults(tmp.path(), V).unwrap();

    assert_eq!(report.unmanaged, vec!["mine.toml".to_string()]);
    assert!(
        report.deleted.is_empty(),
        "a marker in a non-toml file means nothing"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("mine.toml")).unwrap(),
        "x = 1\n"
    );
    assert!(tmp.path().join("notes.txt").exists());
}

#[test]
fn a_read_only_parent_is_an_error() {
    if is_root() {
        eprintln!("skipping: root ignores directory permissions");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let parent = tmp.path().join("beachcomber");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();

    let err = write_managed_defaults(&parent.join("defaults"), V).unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn comb_daemon_refuses_to_start_when_the_defaults_directory_is_unwritable() {
    if is_root() {
        eprintln!("skipping: root ignores directory permissions");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let cfg_dir = tmp.path().join("beachcomber");
    std::fs::create_dir(&cfg_dir).unwrap();
    std::fs::set_permissions(&cfg_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_comb"))
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path())
        .args(["daemon", "--exit-with-parent", "--socket"])
        .arg(tmp.path().join("sock"))
        .output()
        .expect("run comb daemon");
    std::fs::set_permissions(&cfg_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(out.status.code(), Some(2), "no daemon: exit 2");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("could not write managed defaults")
            && stderr.contains(&cfg_dir.join("defaults").display().to_string()),
        "the error names the directory, got:\n{stderr}"
    );
    assert!(
        !tmp.path().join("sock").exists(),
        "it never bound its socket"
    );
}

// ── every shipped template compiles ──────────────────────────────────────────

/// A daemon that holds exactly one template, at `t`, and nothing else.
struct OneTemplate(String);

impl libbeachcomber::resolve::Fetch for OneTemplate {
    fn get(
        &mut self,
        key: &str,
        _path: Option<&str>,
        _force: bool,
        _wait: bool,
    ) -> Result<Option<libbeachcomber::resolve::Got>, libbeachcomber::resolve::FetchError> {
        Ok((key == "t").then(|| libbeachcomber::resolve::Got {
            template: Some(json!(self.0)),
            ..Default::default()
        }))
    }
}

#[test]
fn every_shipped_template_is_a_single_tag_that_compiles() {
    use libbeachcomber::eval::{Form, classify};
    use libbeachcomber::resolve::{Inputs, ResolveError, Slot, resolve};

    let env = HashMap::new();
    let inputs = Inputs {
        cwd: "/",
        env: &env,
        slot: Slot::Global,
        force: false,
        wait: false,
        allow_exec: false,
    };
    let mut checked = 0;
    for (name, body) in SHIPPED {
        let parsed: toml::Value = toml::from_str(body).unwrap_or_else(|e| panic!("{name}: {e}"));
        let providers = parsed["providers"].as_table().unwrap();
        assert_eq!(providers.len(), 1, "{name}: one facet table per file");
        for (namespace, table) in providers {
            assert_eq!(
                namespace,
                &format!("{}:template", name.trim_end_matches(".toml")),
                "{name}: the table is the namespace's template facet"
            );
            for (field, leaf) in table.as_table().unwrap() {
                let src = leaf
                    .as_str()
                    .unwrap_or_else(|| panic!("{name}: {field} is not a string"));
                assert_eq!(classify(src), Form::SingleTag, "{name}: {field} = {src}");
                // Compiling is the question; a runtime failure (here, every
                // exec() call, since exec is off) is a different answer from a
                // template that does not compile.
                match resolve("t", &inputs, &mut OneTemplate(src.to_string())) {
                    Ok(_) | Err(ResolveError::Runtime(_)) => {}
                    Err(e) => panic!("{name}: {field} does not compile: {e}"),
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 13, "every shipped template is covered");
}

// ── the daemon seeds them as the lowest layer ────────────────────────────────

async fn send_recv(
    stream: &mut tokio::net::UnixStream,
    request: serde_json::Value,
) -> serde_json::Value {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(&mut *stream);
    let mut resp = String::new();
    reader.read_line(&mut resp).await.unwrap();
    serde_json::from_str(&resp).unwrap()
}

async fn data(stream: &mut tokio::net::UnixStream, key: &str) -> serde_json::Value {
    let resp = send_recv(stream, json!({"op": "get", "key": key})).await;
    assert_eq!(resp["ok"], json!(true), "get {key}: {resp}");
    resp.get("data").cloned().unwrap_or(serde_json::Value::Null)
}

fn shipped_leaf(name: &str, field: &str) -> String {
    let body = SHIPPED.iter().find(|(n, _)| *n == name).unwrap().1;
    let parsed: toml::Value = toml::from_str(body).unwrap();
    parsed["providers"][format!("{}:template", name.trim_end_matches(".toml"))][field]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_seeds_defaults_beneath_config_and_conf_d() {
    let home = TempDir::new().unwrap();
    let cfg = TempDir::new().unwrap();
    // SAFETY: nextest gives every test its own process, so no other thread is
    // reading the environment concurrently.
    unsafe { std::env::set_var("HOME", home.path().canonicalize().unwrap()) };
    unsafe { std::env::set_var("XDG_CONFIG_HOME", cfg.path()) };
    let cfg_dir = cfg.path().join("beachcomber");
    std::fs::create_dir_all(cfg_dir.join("conf.d")).unwrap();
    std::fs::write(cfg_dir.join("config.toml"), "").unwrap();
    std::fs::write(
        cfg_dir.join("conf.d").join("10-mine.toml"),
        "[providers.\"python:template\"]\nversion = \"{{ env.MY_PY }}\"\n",
    )
    .unwrap();
    write_managed_defaults(&cfg_dir.join("defaults"), V).unwrap();

    let sock_dir = TempDir::new().unwrap();
    let sock = sock_dir.path().join("t.sock");
    let (_handle, ready) = beachcomber::daemon::start_in_process_polling(
        sock.clone(),
        beachcomber::config::Config::load(),
    );
    ready.await.expect("daemon ready");
    let mut s = tokio::net::UnixStream::connect(&sock).await.unwrap();

    let label = |rel: &str| json!(format!("config:{}", cfg_dir.join(rel).display()));
    assert_eq!(
        data(&mut s, "python:template.venv_name").await,
        json!(shipped_leaf("python.toml", "venv_name")),
        "a leaf only the defaults set is the shipped template"
    );
    assert_eq!(
        data(&mut s, "python:template.venv_name:source").await,
        label("defaults/python.toml"),
        "labelled with the defaults file that set it"
    );
    assert_eq!(
        data(&mut s, "python:template.version").await,
        json!("{{ env.MY_PY }}"),
        "a conf.d drop-in overrides a default"
    );
    assert_eq!(
        data(&mut s, "python:template.version:source").await,
        label("conf.d/10-mine.toml"),
    );
    assert_eq!(
        data(&mut s, "kubecontext:template.context").await,
        json!(shipped_leaf("kubecontext.toml", "context")),
        "the exec-backed defaults are seeded as text like any other"
    );
}

// ── comb check config reports the directory ──────────────────────────────────

#[test]
fn comb_check_config_reports_the_defaults_directory() {
    let exe = env!("CARGO_BIN_EXE_comb");
    let tmp = TempDir::new().unwrap();
    let defaults = tmp.path().join("beachcomber").join("defaults");
    let sock = tmp.path().join("sock");

    // Before any daemon has run: the directory is empty, and the check says so
    // without writing anything into it.
    let out = std::process::Command::new(exe)
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path())
        .env("BEACHCOMBER_SOCKET", &sock)
        .args(["check", "--connect-timeout", "0", "config"])
        .output()
        .expect("run comb check config");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!(
            "  [WARN] local    defaults {}: 8 shipped file(s) missing or stale — the daemon \
             writes them on its next start\n",
            defaults.display()
        )),
        "got:\n{stdout}"
    );
    assert!(
        beachcomber::config::Config::conf_d_files(&defaults).is_empty(),
        "the check wrote nothing"
    );

    // After a daemon has started: every shipped file is present at this version.
    let mut daemon = std::process::Command::new(exe)
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path())
        .args(["daemon", "--exit-with-parent", "--socket"])
        .arg(&sock)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn daemon");
    common::poll::poll_until(
        "the spawned `comb daemon`'s socket path",
        "the daemon to have bound its socket",
        Duration::from_secs(5),
        Duration::from_millis(20),
        || sock.exists(),
        |bound| *bound,
    );
    let out = std::process::Command::new(exe)
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path())
        .env("BEACHCOMBER_SOCKET", &sock)
        .args(["check", "--connect-timeout", "2s", "config"])
        .output()
        .expect("run comb check config");
    let _ = daemon.kill();
    let _ = daemon.wait();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!(
            "  [PASS] local    defaults {} (8 files, comb {})\n",
            defaults.display(),
            env!("BEACHCOMBER_VERSION")
        )),
        "got:\n{stdout}"
    );
    assert_eq!(out.status.code(), Some(0), "stdout:\n{stdout}");
    assert_eq!(
        beachcomber::config::Config::conf_d_files(&defaults).len(),
        8,
        "the daemon wrote the shipped set"
    );
}

// ── comb init --write-config is gone ─────────────────────────────────────────

#[test]
fn comb_init_write_config_is_a_clap_error() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_comb"))
        .env("HOME", Path::new("/nonexistent"))
        .env("XDG_CONFIG_HOME", Path::new("/nonexistent"))
        .args(["init", "--write-config"])
        .output()
        .expect("run comb init");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unexpected argument"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
