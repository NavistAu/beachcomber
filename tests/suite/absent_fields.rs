//! A field a Source's run left absent is absent until that Source runs again,
//! exactly as a field it produced is present until then: a read of it is
//! answered from the last run, not by running the Source again.

use crate::common::daemon::TestDaemon;
use crate::common::socket::IsolatedSocket;

/// A daemon with one config Source, `s.main`, that counts its runs into
/// `<home>/runs` and declares `present` (set) and `never` (always absent).
fn counting_daemon() -> (TestDaemon, std::path::PathBuf) {
    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    std::fs::create_dir_all(xdg.join("beachcomber")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let runs = home.join("runs");
    std::fs::write(
        xdg.join("beachcomber").join("config.toml"),
        format!(
            r#"
[daemon.providers.s.sources.main]
data = "{{{{ exec(['sh', '-c', 'echo run >> {runs}; echo 1']) }}}}"
result.present = "{{{{ data }}}}"
result.never = "{{{{ none }}}}"
"#,
            runs = runs.display()
        ),
    )
    .unwrap();
    let d = TestDaemon::spawn_in(socket, home, xdg);
    (d, runs)
}

fn runs(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
fn an_absent_field_is_answered_from_the_last_run() {
    let (d, count) = counting_daemon();
    let cwd = d.home.clone();

    d.comb(&cwd).args(["get", "s.present"]).assert().success();
    assert_eq!(runs(&count), 1, "the cold read runs the Source once");

    for _ in 0..4 {
        d.comb(&cwd)
            .args(["get", "s.never"])
            .assert()
            .success()
            .stdout(predicates::str::is_empty());
    }
    assert_eq!(
        runs(&count),
        1,
        "the last run left `never` absent; reading it runs nothing"
    );
}
