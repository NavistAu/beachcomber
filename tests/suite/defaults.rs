//! Managed defaults (spec §3.3): the shipped templates are files the daemon
//! writes to `<config-dir>/defaults/` on start and seeds as the lowest layer.
//!
//! The writer is exercised directly against a temp directory; the daemon-level
//! tests own their `$HOME` and `$XDG_CONFIG_HOME` (nextest runs each test in
//! its own process, the assumption `tests/suite/seed.rs` makes) so nothing lands in
//! the developer's real `~/.config/beachcomber`.

use crate::common::daemon::isolate;
use beachcomber::daemon::defaults::{
    MARKER_PREFIX, Report, SHIPPED, VERSION, is_current, plan, write_managed_defaults,
};
use serde_json::json;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
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
            files: SHIPPED.len(),
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
        "{}\n[client.providers.python]\ntemplate.version = \"mine\"\n",
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
    assert_eq!(
        report.written.len(),
        SHIPPED.len() - 1,
        "every other one is written"
    );
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

    let out = isolate(
        &mut std::process::Command::new(env!("CARGO_BIN_EXE_comb")),
        tmp.path(),
        tmp.path(),
        &tmp.path().join("sock"),
    )
    .args(["daemon", "--exit-with-parent", "--socket"])
    .arg(tmp.path().join("sock"))
    .output()
    .expect("run comb daemon");
    std::fs::set_permissions(&cfg_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(out.status.code(), Some(2), "no daemon: exit 2");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("error: daemon: could not write managed defaults")
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
        _wait: libbeachcomber::Wait,
    ) -> Result<Option<libbeachcomber::resolve::Got>, libbeachcomber::resolve::FetchError> {
        Ok((key == "t").then(|| libbeachcomber::resolve::Got {
            template: Some(json!(self.0)),
            ..Default::default()
        }))
    }
    fn memo(
        &mut self,
        _write: &libbeachcomber::resolve::MemoWrite,
    ) -> Result<(), libbeachcomber::resolve::FetchError> {
        Ok(())
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
        wait: libbeachcomber::Wait::Budget,
        allow_exec: false,
        now: std::time::SystemTime::now(),
    };
    let mut checked = 0;
    for (name, body) in SHIPPED {
        let parsed: toml::Value = toml::from_str(body).unwrap_or_else(|e| panic!("{name}: {e}"));
        // `00-comb.toml` holds the decay prototype, which is data, not a
        // template; its own tests are below. A file of Sources alone
        // (`hostname.toml`) has no templates.
        let Some(providers) = parsed
            .get("client")
            .and_then(|c| c.get("providers"))
            .and_then(|p| p.as_table())
        else {
            assert!(
                *name == "00-comb.toml" || parsed.get("daemon").is_some(),
                "{name}: templates live in [client.providers]"
            );
            continue;
        };
        assert_eq!(providers.len(), 1, "{name}: one provider per file");
        for (provider, table) in providers {
            assert_eq!(
                provider,
                name.trim_end_matches(".toml"),
                "{name}: the table names the file's provider"
            );
            let table = table.as_table().unwrap();
            assert_eq!(
                table.keys().collect::<Vec<_>>(),
                ["template"],
                "{name}: a client provider holds only its template"
            );
            for (field, leaf) in table["template"].as_table().unwrap() {
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

/// A shipped Source is documented where it is written: the line under its
/// table header is a comment saying what it reads and why it runs when it
/// does, as a user reading `defaults/` would want.
#[test]
fn every_shipped_source_says_what_it_reads() {
    let mut sources = 0;
    for (name, body) in SHIPPED {
        let lines: Vec<&str> = body.lines().collect();
        for (at, line) in lines.iter().enumerate() {
            if line.starts_with("[daemon.providers.") && line.contains(".sources.") {
                assert!(
                    lines.get(at + 1).is_some_and(|next| next.starts_with("# ")),
                    "{name}: {line} has no comment under it"
                );
                sources += 1;
            }
        }
    }
    assert!(sources >= 5, "the shipped Sources are found: {sources}");
}

/// The file-versus-constant pin: the decay the shipped prototype writes is the
/// one `lifecycle::DEFAULT_DECAY` is, so a daemon with managed defaults and an
/// in-process one with none agree.
#[test]
fn the_shipped_default_matches_default_decay() {
    let (_, body) = SHIPPED
        .iter()
        .find(|(name, _)| *name == "00-comb.toml")
        .expect("00-comb.toml is shipped");
    let parsed: toml::Value = toml::from_str(body).unwrap();
    let decay = parsed["daemon"]["providers"]["_"]["cache"]["_"]["decay"].as_integer();
    assert_eq!(
        decay,
        Some(i64::from(beachcomber::scheduler::lifecycle::DEFAULT_DECAY))
    );
}

/// `00-comb.toml` holds the abstract provider's three lines, the decay
/// prototype, and the abstract Source's schedule.
#[test]
fn the_shipped_comb_file_holds_the_abstract_provider_the_decay_and_the_abstract_source() {
    let (_, body) = SHIPPED
        .iter()
        .find(|(name, _)| *name == "00-comb.toml")
        .expect("00-comb.toml is shipped");
    let parsed: toml::Value = toml::from_str(body).unwrap();
    let providers = parsed["daemon"]["providers"]["_"].as_table().unwrap();
    assert_eq!(
        providers.keys().collect::<Vec<_>>(),
        vec!["cache", "result", "return", "sources", "template"],
        "the abstract provider's three lines and its two prototypes"
    );
    assert_eq!(
        providers["template"].as_str(),
        Some("{{ client.providers[_.name].template }}")
    );
    assert_eq!(providers["result"].as_str(), Some("{{ merge(sources) }}"));
    assert_eq!(
        providers["return"].as_str(),
        Some("{{ result | facet('template', template) }}")
    );
    assert_eq!(
        providers["sources"]["_"]["return"].as_str(),
        Some("{{ result | poll('60s', 12) | failback('1s', 3) }}")
    );
    assert_eq!(providers["sources"]["_"].as_table().unwrap().len(), 1);
    assert!(
        body.find("[daemon.providers._]").unwrap()
            < body.find("[daemon.providers._.cache._]").unwrap()
            && body.find("[daemon.providers._.cache._]").unwrap()
                < body.find("[daemon.providers._.sources._]").unwrap(),
        "the provider's block is above the decay block, and the Source's block below it"
    );
}

/// Every shipped file is a layer, labelled as the file the daemon writes it to
/// would be and in the order the daemon composes them (`00-comb.toml`, the
/// abstract provider, first), and the set loads with no error or warning: the
/// floor every test, bench and embedder's registry stands on. It has every
/// provider the Rust built-ins have, and the providers the files add.
#[test]
fn shipped_layers_parse_and_load_with_no_errors() {
    use beachcomber::provider::registry::ProviderRegistry;
    let layers = beachcomber::daemon::defaults::shipped_layers();
    let labels: Vec<String> = layers
        .iter()
        .map(|(path, _)| path.display().to_string())
        .collect();
    let files: Vec<String> = SHIPPED
        .iter()
        .map(|(name, _)| format!("/defaults/{name}"))
        .collect();
    assert_eq!(labels, files);
    assert_eq!(layers[0], beachcomber::daemon::defaults::comb_layer());

    let load = ProviderRegistry::load(
        &beachcomber::config::Config::from_layers(layers),
        &std::env::vars().collect(),
    );
    assert_eq!(load.errors, Vec::<String>::new());
    assert_eq!(load.warnings, Vec::<String>::new());
    let shipped = crate::common::registry::with_shipped_defaults();
    for name in ProviderRegistry::with_defaults().provider_names() {
        assert!(shipped.is_registered(&name), "{name}");
    }
    for name in [
        "asdf",
        "aws",
        "aws_profiles",
        "battery",
        "conda",
        "direnv",
        "gcloud",
        "gcloud_configs",
        "hostname",
        "kubecontext",
        "load",
        "mise",
        "network",
        "op",
        "python",
        "sudo",
        "talos",
        "terraform",
        "uname",
        "uptime",
        "user",
    ] {
        assert!(shipped.is_registered(name), "{name}, from its shipped file");
    }
}

/// Seeding the managed defaults stores the shipped `*:cache` prototype, labelled
/// with the file it came from, and a provider with none of its own resolves
/// decay 4 through it.
#[test]
fn the_shipped_cache_prototype_is_the_decay_default() {
    let home = TempDir::new().unwrap();
    let cfg = TempDir::new().unwrap();
    // SAFETY: nextest gives every test its own process, so no other thread is
    // reading the environment concurrently.
    unsafe { std::env::set_var("HOME", home.path().canonicalize().unwrap()) };
    unsafe { std::env::set_var("XDG_CONFIG_HOME", cfg.path()) };
    let cfg_dir = cfg.path().join("beachcomber");
    write_managed_defaults(&cfg_dir.join("defaults"), V).unwrap();
    std::fs::write(cfg_dir.join("config.toml"), "").unwrap();

    let cache = beachcomber::cache::Cache::new();
    let registry = crate::common::registry::with_shipped_defaults();
    let config = beachcomber::config::Config::load();
    beachcomber::daemon::seed::seed_facets(&cache, &registry, config.composed_layers());
    // `resolve_decay` alone would pass with nothing seeded (it falls back to
    // `DEFAULT_DECAY`), so assert the record itself and its file label.
    let rows: Vec<_> = cache
        .list_rows()
        .into_iter()
        .filter(|r| r.provider == "*:cache" && r.field == "_")
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "00-comb.toml seeds the *:cache `_` prototype: {rows:?}"
    );
    assert!(
        rows[0].source.starts_with("config:") && rows[0].source.ends_with("defaults/00-comb.toml"),
        "stored with the defaults file's label, got {}",
        rows[0].source
    );
    assert_eq!(rows[0].value, json!({"decay": 4}));
    assert_eq!(cache.resolve_decay("hostname", None), 4);
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

/// The `source` label of one `comb status` row.
async fn row_source(stream: &mut tokio::net::UnixStream, provider: &str, field: &str) -> String {
    let resp = send_recv(stream, json!({"op": "status"})).await;
    assert_eq!(resp["ok"], json!(true), "status: {resp}");
    resp["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["provider"] == json!(provider) && r["field"] == json!(field))
        .unwrap_or_else(|| panic!("no status row for {provider}.{field}"))["source"]
        .as_str()
        .unwrap()
        .to_string()
}

fn shipped_leaf(name: &str, field: &str) -> String {
    let body = SHIPPED.iter().find(|(n, _)| *n == name).unwrap().1;
    let parsed: toml::Value = toml::from_str(body).unwrap();
    parsed["client"]["providers"][name.trim_end_matches(".toml")]["template"][field]
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
        "[client.providers.python]\ntemplate.version = \"{{ env.MY_PY }}\"\n",
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

    assert_eq!(
        data(&mut s, "python:template.venv_name").await,
        json!(shipped_leaf("python.toml", "venv_name")),
        "a leaf only the defaults set is the shipped template"
    );
    assert_eq!(
        data(&mut s, "python:template.version").await,
        json!("{{ env.MY_PY }}"),
        "a conf.d drop-in overrides a default"
    );
    let venv_source = row_source(&mut s, "python:template", "venv_name").await;
    assert!(
        venv_source.starts_with("config(") && venv_source.ends_with("defaults/python.toml)"),
        "the shipped leaf is labelled with the defaults file, got {venv_source}"
    );
    // Both files keep a `version` record; the read answers from the drop-in,
    // and the default's is the layer it overrides.
    let resp = send_recv(&mut s, json!({"op": "layers", "key": "python.version"})).await;
    let labels: Vec<(&str, bool)> = resp["data"]
        .as_array()
        .unwrap_or_else(|| panic!("layers: {resp}"))
        .iter()
        .map(|l| {
            (
                l["label"].as_str().unwrap_or_default(),
                l["answered"] == json!(true),
            )
        })
        .collect();
    assert!(
        matches!(labels.as_slice(), [(first, true), (second, false)]
            if first.ends_with("conf.d/10-mine.toml)") && second.ends_with("defaults/python.toml)")),
        "the overriding leaf answers from the drop-in, over the default: {labels:?}"
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
    let out = isolate(
        &mut std::process::Command::new(exe),
        tmp.path(),
        tmp.path(),
        &sock,
    )
    .args(["check", "--connect-timeout", "0", "config"])
    .output()
    .expect("run comb check config");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!(
            "  [WARN] local    defaults {}: {} shipped file(s) missing or stale — the daemon \
             writes them on its next start\n",
            "~/beachcomber/defaults",
            SHIPPED.len()
        )),
        "got:\n{stdout}"
    );
    assert!(!defaults.exists(), "the check created nothing");

    // After a daemon has started: every shipped file is present at this version.
    let mut cmd = std::process::Command::new(exe);
    isolate(&mut cmd, tmp.path(), tmp.path(), &sock)
        .args(["daemon", "--exit-with-parent", "--socket"])
        .arg(&sock)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut daemon = crate::common::daemon::launch(&mut cmd);
    let out = isolate(
        &mut std::process::Command::new(exe),
        tmp.path(),
        tmp.path(),
        &sock,
    )
    .args(["check", "--connect-timeout", "2s", "config"])
    .output()
    .expect("run comb check config");
    crate::common::daemon::stop(&mut daemon);

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!(
            "  [PASS] local    defaults {} ({} files, comb {})\n",
            "~/beachcomber/defaults",
            SHIPPED.len(),
            env!("BEACHCOMBER_VERSION")
        )),
        "got:\n{stdout}"
    );
    assert_eq!(out.status.code(), Some(0), "stdout:\n{stdout}");
    assert_eq!(
        beachcomber::config::Config::conf_d_files(&defaults).len(),
        SHIPPED.len(),
        "the daemon wrote the shipped set"
    );
}

/// A check leaves no state behind. The writability probe lands in the nearest
/// existing ancestor when `defaults/` is absent, so a config directory that has
/// never seen a daemon gets the stale WARN, no FAIL, and no new directory.
#[test]
fn comb_check_config_probes_without_creating_the_defaults_directory() {
    let tmp = TempDir::new().unwrap();
    let cfg_dir = tmp.path().join("beachcomber");
    std::fs::create_dir_all(cfg_dir.join("conf.d")).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        "[daemon]\nlog_level = \"info\"\n",
    )
    .unwrap();

    let out = isolate(
        &mut std::process::Command::new(env!("CARGO_BIN_EXE_comb")),
        tmp.path(),
        tmp.path(),
        &tmp.path().join("no-daemon.sock"),
    )
    .args(["check", "--connect-timeout", "0", "config"])
    .output()
    .expect("run comb check config");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("  [WARN] local    defaults "),
        "the stale WARN is reported, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("not writable"),
        "a writable parent is not a FAIL, got:\n{stdout}"
    );
    assert!(
        !cfg_dir.join("defaults").exists(),
        "the probe must not create defaults/"
    );
    let mut entries: Vec<String> = std::fs::read_dir(&cfg_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        vec!["conf.d", "config.toml"],
        "nothing else appeared — the probe file included"
    );
}

/// `comb check config` with `XDG_CONFIG_HOME` at `root`, no daemon behind the
/// socket, so only the local verdicts are on the line.
fn check_config_stdout(root: &Path) -> String {
    let out = isolate(
        &mut std::process::Command::new(env!("CARGO_BIN_EXE_comb")),
        root,
        root,
        &root.join("no-daemon.sock"),
    )
    .args(["check", "--connect-timeout", "0", "config"])
    .output()
    .expect("run comb check config");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn comb_check_config_counts_the_files_comb_did_not_write() {
    let tmp = TempDir::new().unwrap();
    let defaults = tmp.path().join("beachcomber").join("defaults");
    write_managed_defaults(&defaults, env!("BEACHCOMBER_VERSION")).unwrap();
    std::fs::write(defaults.join("mine.toml"), "x = 1\n").unwrap();

    let stdout = check_config_stdout(tmp.path());

    assert!(
        stdout.contains(&format!(
            "  [WARN] local    defaults {} holds 1 file(s) comb did not write\n",
            "~/beachcomber/defaults"
        )),
        "got:\n{stdout}"
    );
    assert!(
        stdout.contains(&format!("({} files, comb ", SHIPPED.len())),
        "the shipped set is still complete and current, got:\n{stdout}"
    );
}

/// `comb check config` lints what a daemon start would load from `defaults/`:
/// an edited body under this version's marker is loaded as it is, so a broken
/// template in it is a FAIL naming the file; the embedded body is not what is
/// linted.
#[test]
fn comb_check_config_lints_an_edited_current_default_as_the_daemon_loads_it() {
    let tmp = TempDir::new().unwrap();
    let defaults = tmp.path().join("beachcomber").join("defaults");
    write_managed_defaults(&defaults, VERSION).unwrap();
    std::fs::write(
        defaults.join("python.toml"),
        format!(
            "{}\n[client.providers.p]\ntemplate.t = \"{{{{ x( }}}}\"\n",
            marker(VERSION)
        ),
    )
    .unwrap();

    let stdout = check_config_stdout(tmp.path());

    assert!(
        stdout.lines().any(|l| l.contains("[FAIL] local")
            && l.contains("python.toml")
            && l.contains("template.t")),
        "got:\n{stdout}"
    );
}

/// The same for a non-comb `*.toml` in `defaults/`: the daemon loads it, so
/// `check config` lints it (and still warns that comb did not write it).
#[test]
fn comb_check_config_lints_a_non_comb_toml_in_defaults_as_the_daemon_loads_it() {
    let tmp = TempDir::new().unwrap();
    let defaults = tmp.path().join("beachcomber").join("defaults");
    std::fs::create_dir_all(&defaults).unwrap();
    std::fs::write(
        defaults.join("mine.toml"),
        "[client.providers.p]\ntemplate.t = \"{{ x( }}\"\n",
    )
    .unwrap();

    let stdout = check_config_stdout(tmp.path());

    assert!(
        stdout.lines().any(|l| l.contains("[FAIL] local")
            && l.contains("mine.toml")
            && l.contains("template.t")),
        "got:\n{stdout}"
    );
    assert!(
        stdout.contains("holds 1 file(s) comb did not write"),
        "got:\n{stdout}"
    );
}

/// The plan is the daemon's start-up decision, so what it says is loaded
/// before the daemon writes is what is on disk after: same names, same text.
#[test]
fn the_plan_loads_what_the_directory_holds_once_the_daemon_has_written() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let edited = format!("{}\n# edited\n", marker(V));
    std::fs::write(dir.join("python.toml"), &edited).unwrap();
    std::fs::write(dir.join("aws.toml"), "# no marker\n").unwrap();
    std::fs::write(dir.join("mine.toml"), "x = 1\n").unwrap();
    std::fs::write(dir.join("old.toml"), format!("{}\n", marker("0.0.1"))).unwrap();

    let planned = plan(dir, V);
    assert_eq!(names(&planned.retired), ["old.toml"]);
    let before: Vec<(String, String)> = planned
        .loaded()
        .into_iter()
        .map(|(path, text)| {
            let text = text
                .map(str::to_string)
                .unwrap_or_else(|| std::fs::read_to_string(&path).unwrap());
            (
                path.file_name().unwrap().to_str().unwrap().to_string(),
                text,
            )
        })
        .collect();

    write_managed_defaults(dir, V).unwrap();

    let after: Vec<(String, String)> = beachcomber::config::Config::conf_d_files(dir)
        .into_iter()
        .map(|path| {
            (
                path.file_name().unwrap().to_str().unwrap().to_string(),
                std::fs::read_to_string(&path).unwrap(),
            )
        })
        .collect();
    assert_eq!(before, after);
    assert!(
        before
            .iter()
            .any(|(n, t)| n == "python.toml" && *t == edited)
    );
    assert!(
        before
            .iter()
            .any(|(n, t)| n == "mine.toml" && t == "x = 1\n")
    );
    assert!(!before.iter().any(|(n, _)| n == "old.toml"));
}

#[test]
fn comb_check_config_fails_an_unwritable_defaults_directory() {
    if is_root() {
        eprintln!("skipping: root ignores directory permissions");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let defaults = tmp.path().join("beachcomber").join("defaults");
    write_managed_defaults(&defaults, env!("BEACHCOMBER_VERSION")).unwrap();
    std::fs::set_permissions(&defaults, std::fs::Permissions::from_mode(0o555)).unwrap();

    let stdout = check_config_stdout(tmp.path());
    std::fs::set_permissions(&defaults, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(
        stdout.contains(&format!(
            "  [FAIL] local    defaults {} not writable: ",
            "~/beachcomber/defaults"
        )) && stdout.contains("— the daemon cannot rewrite them on upgrade\n"),
        "got:\n{stdout}"
    );
}

// ── comb init --write-config is gone ─────────────────────────────────────────

#[test]
fn comb_init_write_config_is_a_clap_error() {
    let out = isolate(
        &mut std::process::Command::new(env!("CARGO_BIN_EXE_comb")),
        Path::new("/nonexistent"),
        Path::new("/nonexistent"),
        Path::new("/nonexistent/sock"),
    )
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
