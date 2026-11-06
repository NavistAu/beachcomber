//! `defaults/kubecontext.toml` and `defaults/talos.toml`: the tool's context,
//! memoised. Each test counts the processes the shipped templates spawn with
//! stand-in `kubectl` and `talosctl` that log themselves to `$COMB_STUB_LOG`.

use super::{Scenario, scenario};
use crate::common::poll::poll_until;
use crate::common::socket::{ask, watches};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A scenario, and a `bin/` of stand-in tools beside its home: `kubectl`
/// answers `ctx-1` and the namespace `ns-1`; `talosctl` answers the JSON of a
/// config whose context is `admin@talos`.
struct Tools {
    s: Scenario,
    bin: PathBuf,
    log: PathBuf,
}

fn tools() -> Tools {
    let s = scenario();
    let base = s.daemon.home.parent().unwrap().to_path_buf();
    let bin = base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let write = |name: &str, answers: &str| {
        let path = bin.join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho \"{name} $*\" >> \"$COMB_STUB_LOG\"\n{answers}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    write(
        "kubectl",
        "case \"$2\" in current-context) printf 'ctx-1' ;; view) printf 'ns-1' ;; esac",
    );
    write("talosctl", "printf '{\"context\":\"admin@talos\"}'");
    Tools {
        s,
        bin,
        log: base.join("stub.log"),
    }
}

impl Tools {
    /// `comb <args>` from `~`, on this scenario's daemon, the stand-ins
    /// first on `PATH`, and `vars` set. Its stdout; the command must succeed.
    fn comb(&self, args: &[&str], vars: &[(&str, &str)]) -> String {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            crate::common::daemon::stub_bin().display()
        );
        let mut cmd = self.s.daemon.command(&self.s.daemon.home);
        let out = cmd
            .env("PATH", format!("{path}:/usr/bin:/bin"))
            .env("COMB_STUB_LOG", &self.log)
            .envs(vars.iter().copied())
            .args(args)
            .output()
            .expect("run comb");
        assert!(
            out.status.success(),
            "comb {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn get(&self, key: &str, vars: &[(&str, &str)]) -> Value {
        serde_json::from_str(&self.comb(&["get", "-f", "json", key], vars)).unwrap()
    }

    /// The processes the stand-ins have been run as, so far.
    fn runs(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn home(&self) -> &Path {
        &self.s.daemon.home
    }

    /// The record of the memo of `kubecontext.context` (`:cache`).
    fn memo(&self) -> Value {
        let get = json!({"op": "get", "key": "kubecontext:cache.context"});
        ask(&self.s.daemon.socket.path, get)["data"].clone()
    }

    /// Write `body` to `path`, a file the memo watches, and wait until the
    /// daemon has heard it: the memo's hash is gone. The watcher hands a
    /// change over a moment after it is made, and a hash dropped after the
    /// next read wrote a new memo makes the read after that run again.
    fn rewrite(&self, path: &Path, body: &str) {
        let sock = &self.s.daemon.socket.path;
        // Every path the memo names is watched first, so the write is an event.
        poll_until(
            "the memo and introspect watches",
            "a hash, and each path the memo watches Active",
            Duration::from_secs(10),
            Duration::from_millis(5),
            || (self.memo(), watches(sock)),
            |(memo, watched)| {
                memo.get("hash").is_some()
                    && memo["watch"].as_array().is_some_and(|paths| {
                        paths.iter().all(|p| {
                            watched.iter().any(|(w, state)| {
                                Some(w.as_str()) == p.as_str() && state["state"] == "active"
                            })
                        })
                    })
            },
        );
        std::fs::write(path, body).unwrap();
        poll_until(
            "kubecontext:cache.context",
            "the memo's record without its hash",
            Duration::from_secs(10),
            Duration::from_millis(5),
            || self.memo(),
            |memo| memo.get("hash").is_none(),
        );
    }
}

/// The second read of a context runs nothing.
#[test]
fn kubecontext_context_runs_kubectl_once() {
    let t = tools();
    for _ in 0..3 {
        assert_eq!(t.get("kubecontext.context", &[]), json!("ctx-1"));
    }
    assert_eq!(t.runs().len(), 1, "{:?}", t.runs());
}

/// Naming other kubeconfig files runs it again; so does a change to a file
/// the list names, and so does one appearing.
#[test]
fn kubecontext_runs_again_when_kubeconfig_changes() {
    let t = tools();
    let (a, b) = (t.home().join("a"), t.home().join("b"));
    let list = a.display().to_string();
    let with = |l: &str| [("KUBECONFIG", l.to_string())];
    let get = |l: &str| {
        let v = with(l);
        t.get("kubecontext.context", &[(v[0].0, v[0].1.as_str())])
    };
    assert_eq!(get(&list), json!("ctx-1"));
    assert_eq!(t.runs().len(), 1);
    let both = format!("{}:{}", a.display(), b.display());
    get(&both);
    assert_eq!(t.runs().len(), 2, "another KUBECONFIG");
    get(&both);
    assert_eq!(t.runs().len(), 2);
    t.rewrite(&b, "contexts: []");
    get(&both);
    assert_eq!(t.runs().len(), 3, "a file of the list appeared");
    t.rewrite(&b, "contexts: [one]");
    get(&both);
    assert_eq!(t.runs().len(), 4, "a file of the list was rewritten");
    get(&both);
    assert_eq!(t.runs().len(), 4);
}

/// With no `KUBECONFIG`, `~/.kube/config` is the file watched.
#[test]
fn kubecontext_watches_the_default_kubeconfig() {
    let t = tools();
    t.get("kubecontext.context", &[]);
    t.get("kubecontext.context", &[]);
    assert_eq!(t.runs().len(), 1);
    let config = t.home().join(".kube/config");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "current-context: ctx-1").unwrap();
    t.get("kubecontext.context", &[]);
    assert_eq!(t.runs().len(), 2);
}

/// The namespace reads the context, and both leaves are memoised: the pair
/// costs `kubectl` twice, once each, and then nothing.
#[test]
fn the_namespace_and_the_context_cost_two_runs() {
    let t = tools();
    assert_eq!(t.get("kubecontext.namespace", &[]), json!("ns-1"));
    assert_eq!(t.runs().len(), 2, "{:?}", t.runs());
    assert_eq!(t.get("kubecontext.context", &[]), json!("ctx-1"));
    assert_eq!(t.get("kubecontext.namespace", &[]), json!("ns-1"));
    assert_eq!(t.runs().len(), 2, "{:?}", t.runs());
}

/// The talos context is memoised like kubectl's, keyed on `TALOSCONFIG`.
#[test]
fn talos_context_runs_talosctl_once() {
    let t = tools();
    for _ in 0..3 {
        assert_eq!(t.get("talos.context", &[]), json!("admin@talos"));
    }
    assert_eq!(t.runs().len(), 1);
    let other = t.home().join("talosconfig");
    t.get("talos.context", &[("TALOSCONFIG", other.to_str().unwrap())]);
    assert_eq!(t.runs().len(), 2, "another TALOSCONFIG");
}

/// Once warm, `comb status` over the shipped defaults spawns nothing, with a
/// context set and with none.
#[test]
fn a_warm_status_spawns_no_process() {
    let t = tools();
    t.comb(&["status"], &[]);
    let cold = t.runs().len();
    assert!(cold > 0, "the first status ran the tools");
    t.comb(&["status"], &[]);
    t.comb(&["status"], &[]);
    assert_eq!(t.runs().len(), cold, "{:?}", t.runs());
}

/// Nothing set is remembered too: a `kubectl` with no context runs once.
#[test]
fn no_context_is_remembered() {
    let t = tools();
    let kubectl = t.bin.join("kubectl");
    std::fs::write(
        &kubectl,
        "#!/bin/sh\necho \"kubectl $*\" >> \"$COMB_STUB_LOG\"\nexit 1\n",
    )
    .unwrap();
    for _ in 0..3 {
        assert_eq!(
            t.comb(&["get", "-f", "json", "kubecontext.context"], &[]),
            "null\n"
        );
        assert_eq!(
            t.comb(&["get", "-f", "json", "kubecontext.namespace"], &[]),
            "null\n"
        );
    }
    assert_eq!(t.runs().len(), 1, "{:?}", t.runs());
}
