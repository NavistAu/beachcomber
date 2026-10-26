//! Caching a template end to end, over the CLI: one cached leaf whose command
//! is the counting stub `kubectl`, read by `comb get`, `comb status`,
//! `--force`, `comb refresh`, `--no-exec` and `comb eval`. How many times the
//! command ran is what each test asserts.
//!
//! The daemon runs in this process, and nothing of the memo's lifecycle is
//! awaited: a read that runs the command again found the change itself.

use crate::common::daemon::TestDaemon;
use crate::common::socket::IsolatedSocket;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Output;
use tempfile::TempDir;

/// `p.f` runs `kubectl x` (the stub fails, so the `echo` answers) once per
/// change of `~/state` and at least hourly; `p.gone` renders nothing, which is
/// remembered too.
const CONFIG: &str = r#"
[client.providers.p]
template.f = "{{ 'kubectl x || echo prod' | watch('~/state') | ttl('1h') | cache() | exec() }}"
template.gone = "{{ 'kubectl y || true' | cache() | exec() or none }}"
template.kube = "{{ 'kubectl config current-context 2>/dev/null || true' | watch((env.KUBECONFIG or '~/.kube/config') | split(':')) | ttl('10m') | cache(env.KUBECONFIG) | exec() or none }}"
"#;

struct Cached {
    d: TestDaemon,
    home: PathBuf,
    _base: TempDir,
}

fn cached() -> Cached {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().canonicalize().unwrap();
    let (home, xdg) = (root.join("home"), root.join("xdg"));
    let config = xdg.join("beachcomber").join("config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, CONFIG).unwrap();
    let d = TestDaemon::serve_in_process(IsolatedSocket::new(), home.clone(), xdg);
    Cached {
        d,
        home,
        _base: base,
    }
}

impl Cached {
    /// `comb <args>` at `~`, its stub tools counting themselves, and no
    /// `KUBECONFIG` of the developer's.
    fn comb(&self, args: &[&str]) -> Output {
        self.d
            .command(&self.home)
            .env("COMB_STUB_LOG", self.home.join("stub.log"))
            .env_remove("KUBECONFIG")
            .args(args)
            .output()
            .expect("run comb")
    }

    /// `comb <args>`'s output, which must succeed.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.comb(args);
        assert!(
            out.status.success(),
            "comb {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// `comb get -f json <key>`'s value.
    fn json(&self, key: &str) -> Value {
        serde_json::from_str(&self.ok(&["get", "-f", "json", key])).unwrap()
    }

    /// How many times `kubectl <args>` has run.
    fn runs(&self, args: &str) -> usize {
        std::fs::read_to_string(self.home.join("stub.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| *l == format!("kubectl {args}"))
            .count()
    }
}

/// The first read runs the command and stores what it printed; every read
/// after it is served the memo, prints the same, and runs nothing.
#[test]
fn a_cached_leaf_runs_its_command_once() {
    let c = cached();
    let first = c.ok(&["get", "p.f"]);
    assert_eq!(first, "prod");
    for _ in 1..20 {
        assert_eq!(c.ok(&["get", "p.f"]), first);
    }
    assert_eq!(c.runs("x"), 1);
    let record = c.json("p:cache.f");
    assert_eq!(record["source"], "memo", "{record}");
    assert_eq!(record["stale"], false, "{record}");
    assert_eq!(record["watch"], json!([c.home.join("state")]), "{record}");
    assert_eq!(record["poll"]["count"], 1, "{record}");
    assert_eq!(record["decay"], 1, "{record}");
    let hash = record["hash"].as_str().unwrap();
    assert!(
        hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "{record}"
    );
    assert_eq!(c.json("p:memo.f"), json!("prod"));
}

/// The reader stamps the watched file: its appearing, and each rewrite, are a
/// new hash, so the next read runs the command, whatever the daemon has heard.
#[test]
fn a_change_to_a_watched_file_runs_it_again() {
    let c = cached();
    c.ok(&["get", "p.f"]);
    std::fs::write(c.home.join("state"), "1").unwrap();
    c.ok(&["get", "p.f"]);
    c.ok(&["get", "p.f"]);
    assert_eq!(c.runs("x"), 2);
    std::fs::write(c.home.join("state"), "22").unwrap();
    c.ok(&["get", "p.f"]);
    assert_eq!(c.runs("x"), 3);
}

/// `--force` renders past the memo and stores the new one.
#[test]
fn force_runs_it_again() {
    let c = cached();
    c.ok(&["get", "p.f"]);
    c.ok(&["get", "--force", "p.f"]);
    c.ok(&["get", "p.f"]);
    assert_eq!(c.runs("x"), 2);
}

/// `comb refresh p` drops the memo's hash: the next read renders.
#[test]
fn refresh_runs_it_again() {
    let c = cached();
    c.ok(&["get", "p.f"]);
    c.ok(&["refresh", "p"]);
    c.ok(&["get", "p.f"]);
    c.ok(&["get", "p.f"]);
    assert_eq!(c.runs("x"), 2);
}

/// `comb status` serves a warm leaf from its memo, says so, and runs nothing.
#[test]
fn status_serves_the_memo_and_runs_nothing() {
    let c = cached();
    c.ok(&["get", "p.f"]);
    let rows: Vec<Value> = c
        .ok(&["status", "-f", "json", "--filter", "provider=p"])
        .lines()
        .map(|l| serde_json::from_str(l).expect("status -f json is NDJSON"))
        .collect();
    let row = rows
        .iter()
        .find(|r| r["kind"]["kind"] == "rendered" && r["field"] == "f")
        .unwrap_or_else(|| panic!("a rendered row: {rows:?}"));
    assert_eq!(
        (&row["source"], &row["value"]),
        (&json!("memo"), &json!("prod"))
    );
    assert_eq!(c.runs("x"), 1);
}

/// A client that may not run `exec()` is served what one that may stored.
#[test]
fn no_exec_is_served_the_memo() {
    let c = cached();
    c.ok(&["get", "p.f"]);
    assert_eq!(c.ok(&["--no-exec", "get", "p.f"]), "prod");
    assert_eq!(c.runs("x"), 1);
}

/// `comb eval` stores nothing: its own `cache()` passes through, and the
/// command runs on every eval.
#[test]
fn eval_runs_every_time_and_stores_nothing() {
    let c = cached();
    for _ in 0..2 {
        assert_eq!(
            c.ok(&["eval", "{{ 'kubectl z || echo e' | cache() | exec() }}"]),
            "e"
        );
    }
    assert_eq!(c.runs("z"), 2);
}

/// A leaf that renders nothing is remembered as nothing: a miss, printed as
/// `null`, whose command ran once.
#[test]
fn a_memo_of_absence_prints_null_and_runs_once() {
    let c = cached();
    for _ in 0..2 {
        assert_eq!(c.ok(&["get", "-f", "json", "p.gone"]), "null\n");
    }
    assert_eq!(c.runs("y"), 1);
    let record = c.json("p:cache.gone");
    assert_eq!(record["source"], "memo", "{record}");
    assert!(record["hash"].is_string(), "{record}");
}

/// The phase 5 plan's kubecontext pipeline: no context is absence,
/// remembered; each file of a `KUBECONFIG` colon list is watched, and the
/// variable is an input, so naming other files runs it again.
#[test]
fn a_kubeconfig_pipeline_runs_once_per_config() {
    let c = cached();
    for _ in 0..2 {
        assert_eq!(c.ok(&["get", "-f", "json", "p.kube"]), "null\n");
    }
    assert_eq!(c.runs("config current-context"), 1);
    let record = c.json("p:cache.kube");
    assert_eq!(
        record["watch"],
        json!([c.home.join(".kube/config")]),
        "{record}"
    );

    let (a, b) = (c.home.join("a"), c.home.join("b"));
    let list = format!("{}:{}", a.display(), b.display());
    let get = || {
        let out =
            c.d.command(&c.home)
                .env("COMB_STUB_LOG", c.home.join("stub.log"))
                .env("KUBECONFIG", &list)
                .args(["get", "-f", "json", "p.kube"])
                .output()
                .unwrap();
        assert!(out.status.success(), "{out:?}");
    };
    get();
    get();
    assert_eq!(c.runs("config current-context"), 2);
    assert_eq!(c.json("p:cache.kube")["watch"], json!([a, b]));
    std::fs::write(&b, "contexts: []").unwrap();
    get();
    assert_eq!(
        c.runs("config current-context"),
        3,
        "a file of the list appeared"
    );
}

/// `--force` means compute afresh: on a namespace a `put` made, a template
/// renders past its memo, and only an address with no template has nothing
/// to force.
#[test]
fn force_reaches_a_template_a_put_made() {
    let c = cached();
    c.ok(&[
        "put",
        "q:template.f",
        r#""{{ 'kubectl q || echo q' | cache() | exec() }}""#,
    ]);
    c.ok(&["get", "q.f"]);
    assert_eq!(c.ok(&["get", "--force", "q.f"]), "q");
    assert_eq!(c.runs("q"), 2);
    c.ok(&["put", "r.x", "1"]);
    let out = c.comb(&["get", "--force", "r.x"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("cannot --force static namespace 'r': no source to re-execute from"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A reader that waits nothing (`--wait 0`) is served a memo past its ttl and
/// runs nothing; a reader that waits renders it. The memo is aged on the
/// daemon's clock, paused, so the ttl passes without a second of wall time.
#[tokio::test(start_paused = true)]
async fn wait_zero_on_a_stale_memo_runs_nothing() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let sock = home.join("s");
    let watchers = std::sync::Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = std::sync::Arc::new(beachcomber::cache::Cache::with_watchers(watchers.clone()));
    let registry = std::sync::Arc::new(crate::common::registry::with_shipped_defaults());
    beachcomber::server::Server::new(sock.clone(), cache, registry, None, watchers)
        .spawn()
        .expect("the server binds its socket");
    // A `comb` on a thread of its own, so this runtime's one thread serves it.
    let comb = |args: &'static [&'static str]| {
        let (home, sock) = (home.clone(), sock.clone());
        tokio::task::spawn_blocking(move || {
            let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
            crate::common::daemon::isolate(&mut cmd, &home, &home.join("xdg"), &sock)
                .current_dir(&home)
                .env("COMB_STUB_LOG", home.join("stub.log"))
                .args(args);
            let out = cmd.output().expect("run comb");
            assert!(out.status.success(), "comb {args:?}: {out:?}");
            String::from_utf8(out.stdout).unwrap()
        })
    };
    let runs = || {
        std::fs::read_to_string(home.join("stub.log"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    comb(&[
        "put",
        "p:template.f",
        r#""{{ 'kubectl w || echo fresh' | ttl('1s') | cache() | exec() }}""#,
    ])
    .await
    .unwrap();
    assert_eq!(comb(&["get", "p.f"]).await.unwrap(), "fresh");
    assert_eq!(runs(), 1);

    // Past the ttl on the daemon's clock, and on from there in real time.
    tokio::time::advance(std::time::Duration::from_secs(3)).await;
    tokio::time::resume();
    let record: Value =
        serde_json::from_str(&comb(&["get", "-f", "json", "p:cache.f"]).await.unwrap()).unwrap();
    assert_eq!(record["stale"], true, "{record}");

    assert_eq!(comb(&["get", "--wait", "0", "p.f"]).await.unwrap(), "fresh");
    assert_eq!(runs(), 1, "the stale memo, and nothing ran");
    assert_eq!(comb(&["get", "p.f"]).await.unwrap(), "fresh");
    assert_eq!(runs(), 2, "a reader that waits renders it");
}
