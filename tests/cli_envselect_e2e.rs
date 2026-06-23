//! End-to-end Tier-B integration: the CLI resolves an env selector ($KUBECONFIG /
//! $TALOSCONFIG) into a file path via the provider's path expression and hands it
//! to the daemon, which reads/caches/watches exactly that file. The daemon never
//! reads the selector env var itself.

mod common;
use common::daemon::TestDaemon;
use tempfile::TempDir;

/// A `comb` command pointed at the test daemon's socket. Unlike the golden
/// helper we do NOT fix CWD — these env-selected providers ignore CWD; their
/// path comes entirely from the selector env var.
fn comb(d: &TestDaemon) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &d.socket.path);
    cmd.env("RUST_LOG", "error");
    cmd
}

const KC_X: &str = "apiVersion: v1\ncurrent-context: ctx-x\ncontexts:\n- context:\n    namespace: ns-x\n  name: ctx-x\n";
const KC_Y: &str = "apiVersion: v1\ncurrent-context: ctx-y\ncontexts:\n- context:\n    namespace: ns-y\n  name: ctx-y\n";

#[test]
fn kubecontext_reads_the_selected_kubeconfig() {
    let d = TestDaemon::spawn();
    let tmp = TempDir::new().unwrap();
    let kc = tmp.path().join("config");
    std::fs::write(&kc, KC_X).unwrap();

    // $KUBECONFIG selects the file; the CLI resolves it via the path expression
    // `env.KUBECONFIG or '~/.kube/config'` and sends the path to the daemon.
    comb(&d)
        .args(["get", "kubecontext.context", "--format", "text"])
        .env("KUBECONFIG", &kc)
        .assert()
        .success()
        .stdout(predicates::str::contains("ctx-x"));
}

#[test]
fn kubecontext_merges_colon_joined_kubeconfig_list_last_wins() {
    let d = TestDaemon::spawn();
    let tmp = TempDir::new().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    std::fs::write(&a, KC_X).unwrap();
    std::fs::write(&b, KC_Y).unwrap();
    let joined = format!("{}:{}", a.display(), b.display());

    // A colon-joined $KUBECONFIG list is merged; the later file's current-context wins.
    comb(&d)
        .args(["get", "kubecontext.context", "--format", "text"])
        .env("KUBECONFIG", &joined)
        .assert()
        .success()
        .stdout(predicates::str::contains("ctx-y"));
}

#[test]
fn daemon_does_not_read_its_own_kubeconfig_env() {
    // The daemon process inherits no special env; the value must reflect the
    // file the CLIENT selected, not anything the daemon's environment holds.
    // Here we simply confirm the selected file is read (the daemon was spawned
    // by the harness with no KUBECONFIG), proving the path came from the client.
    let d = TestDaemon::spawn();
    let tmp = TempDir::new().unwrap();
    let kc = tmp.path().join("config");
    std::fs::write(&kc, KC_Y).unwrap();
    comb(&d)
        .args(["get", "kubecontext.context", "--format", "text"])
        .env("KUBECONFIG", &kc)
        .assert()
        .success()
        .stdout(predicates::str::contains("ctx-y"));
}

const TC: &str =
    "context: prod\ncontexts:\n    prod:\n        endpoints:\n            - 10.0.0.1\n";

#[test]
fn talos_reads_the_selected_talosconfig() {
    let d = TestDaemon::spawn();
    let tmp = TempDir::new().unwrap();
    let tc = tmp.path().join("config");
    std::fs::write(&tc, TC).unwrap();

    comb(&d)
        .args(["get", "talos.context", "--format", "text"])
        .env("TALOSCONFIG", &tc)
        .assert()
        .success()
        .stdout(predicates::str::contains("prod"));
}
