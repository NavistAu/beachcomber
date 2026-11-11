//! `_` names no field: it is each node's own (`<p>:cache._.decay`, and the
//! metadata `comb get -v` keeps at a node), so a put naming one is refused,
//! as a template or a Source's `result` is at load (their unit tests).

use crate::common::daemon::TestDaemon;
use crate::common::socket::IsolatedSocket;

#[test]
fn a_put_naming_underscore_is_refused() {
    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    std::fs::create_dir_all(xdg.join("beachcomber")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let d = TestDaemon::serve_in(socket, home.clone(), xdg);
    for args in [["put", "u", r#"{"a":{"_":1}}"#], ["put", "u._", "3"]] {
        d.comb(&home)
            .args(args)
            .assert()
            .failure()
            .stderr(predicates::str::contains("'_' is reserved"));
    }
    d.comb(&home)
        .args(["put", "*:cache._.decay", "3"])
        .assert()
        .success();
}
