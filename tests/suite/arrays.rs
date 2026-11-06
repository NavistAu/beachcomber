//! Arrays are values of their own: what is written as a list reads back as a
//! list — through `put`, a Source's result, the wire, every `comb get` format
//! and a template — and its elements are addressed by decimal index. An array
//! is a leaf: a nearer or later layer's array replaces it whole.

use crate::common::daemon::TestDaemon;
use crate::common::socket::IsolatedSocket;
use std::path::{Path, PathBuf};

fn project(d: &TestDaemon) -> PathBuf {
    let dir = d.home.join("ws").join("project");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn put(d: &TestDaemon, cwd: &Path, args: &[&str]) {
    d.comb(cwd).arg("put").args(args).assert().success();
}

/// `comb get -f json <key>`, parsed.
fn json(d: &TestDaemon, cwd: &Path, key: &str) -> serde_json::Value {
    let out = d
        .comb(cwd)
        .args(["get", "-f", "json", key])
        .assert()
        .success();
    serde_json::from_slice(&out.get_output().stdout).expect("stdout is JSON")
}

fn eval(d: &TestDaemon, cwd: &Path, src: &str) -> String {
    let out = d.comb(cwd).args(["eval", src]).assert().success();
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

#[test]
fn a_put_array_reads_back_as_an_array() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(&d, &cwd, &["demo", r#"{"l":[1,2,{"z":3}]}"#]);

    assert_eq!(
        json(&d, &cwd, "demo.l"),
        serde_json::json!([1, 2, {"z": 3}])
    );
    assert_eq!(
        json(&d, &cwd, "demo"),
        serde_json::json!({"l": [1, 2, {"z": 3}]})
    );
    assert_eq!(json(&d, &cwd, "demo.l.1"), serde_json::json!(2));
    assert_eq!(json(&d, &cwd, "demo.l[2].z"), serde_json::json!(3));
}

#[test]
fn a_null_element_is_absent_and_the_list_closes_up() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(&d, &cwd, &["demo.l", "[1,null,3]"]);
    assert_eq!(json(&d, &cwd, "demo.l"), serde_json::json!([1, 3]));
}

#[test]
fn a_template_sees_a_stored_array_as_a_list() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(&d, &cwd, &["demo.l", r#"["a","b","c"]"#]);

    assert_eq!(eval(&d, &cwd, "{{ demo.l[0] }}"), "a");
    assert_eq!(eval(&d, &cwd, "{{ comb.demo.l[1] }}"), "b");
    assert_eq!(eval(&d, &cwd, "{{ demo.l | length }}"), "3");
    assert_eq!(eval(&d, &cwd, "{{ demo.l is sequence }}"), "true");
    assert_eq!(
        eval(&d, &cwd, "{% for x in demo.l %}{{ x }}{% endfor %}"),
        "abc"
    );
}

#[test]
fn a_put_at_an_index_writes_into_the_array() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(&d, &cwd, &["demo.l", "[1,2,3]"]);

    put(&d, &cwd, &["demo.l.1", "9"]);
    assert_eq!(json(&d, &cwd, "demo.l"), serde_json::json!([1, 9, 3]));

    // One past the end appends.
    put(&d, &cwd, &["demo.l[3]", "4"]);
    assert_eq!(json(&d, &cwd, "demo.l"), serde_json::json!([1, 9, 3, 4]));

    // Removing an element closes the list up.
    put(&d, &cwd, &["demo.l.0", "null"]);
    assert_eq!(json(&d, &cwd, "demo.l"), serde_json::json!([9, 3, 4]));
}

#[test]
fn a_nearer_array_replaces_a_farther_one_whole() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(&d, &cwd, &["--global", "demo.l", "[1,2,3]"]);
    put(&d, &cwd, &["demo.l", "[9]"]);
    assert_eq!(json(&d, &cwd, "demo.l"), serde_json::json!([9]));
}

#[test]
fn a_sources_array_result_is_an_array() {
    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    std::fs::create_dir_all(xdg.join("beachcomber")).unwrap();
    std::fs::write(
        xdg.join("beachcomber").join("config.toml"),
        r#"
[daemon.providers.s.sources.main]
data = "{{ [1, 2, 3] }}"
result.list = "{{ data }}"
"#,
    )
    .unwrap();
    let d = TestDaemon::spawn_in(socket, home, xdg);
    let cwd = project(&d);

    assert_eq!(json(&d, &cwd, "s.list"), serde_json::json!([1, 2, 3]));
    assert_eq!(json(&d, &cwd, "s.list.2"), serde_json::json!(3));
}

#[test]
fn status_shows_an_array_as_one_row() {
    let d = TestDaemon::spawn();
    let cwd = project(&d);
    put(&d, &cwd, &["demo.l", "[1,2]"]);

    let out = d
        .comb(&cwd)
        .args(["status", "-f", "json", "--filter", "provider=demo"])
        .assert()
        .success();
    let rows: Vec<serde_json::Value> = String::from_utf8_lossy(&out.get_output().stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0]["field"], "l");
    assert_eq!(rows[0]["value"], serde_json::json!([1, 2]));
}
