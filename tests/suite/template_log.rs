//! `fail()` in a Source's line fails the run as a failed `exec()` does: the
//! last good value stands.

use crate::common::daemon::TestDaemon;
use crate::common::socket::IsolatedSocket;

#[test]
fn a_source_that_fails_keeps_its_last_good_value() {
    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    std::fs::create_dir_all(xdg.join("beachcomber")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let flag = home.join("no-agent");
    std::fs::write(
        xdg.join("beachcomber").join("config.toml"),
        format!(
            r#"
[daemon.providers.s.sources.main]
data = "{{% if '{flag}' | glob %}}{{{{ _.fail('no ssh agent') }}}}{{% endif %}}ok"
result.v = "{{{{ data }}}}"
"#,
            flag = flag.display()
        ),
    )
    .unwrap();
    let d = TestDaemon::serve_in(socket, home.clone(), xdg);

    d.comb(&home)
        .args(["get", "s.v"])
        .assert()
        .success()
        .stdout("ok");
    std::fs::write(&flag, "").unwrap();
    d.comb(&home)
        .args(["get", "--force", "s.v"])
        .assert()
        .success()
        .stdout("ok");
}
