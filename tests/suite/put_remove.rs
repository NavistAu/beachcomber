use crate::common::fetch::fetch;
use beachcomber::client::Client;
use beachcomber::config::Config;

/// Spin up an in-process daemon on a temp socket and return the handle and client.
async fn setup_daemon() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    Client,
    tokio::task::JoinHandle<()>,
) {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let config = Config::default();
    let (handle, ready) = beachcomber::daemon::start_in_process_polling(sock.clone(), config);
    ready.await.expect("daemon ready");
    let client = Client::new(sock.clone());
    (tmp, sock, client, handle)
}

/// A put of null clears a cache entry but the static namespace's registration survives,
/// so a subsequent put under the same key still works.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_of_null_clears_entry_but_keeps_provider() {
    let (_tmp, sock, client, handle) = setup_daemon().await;

    // 1. Store a value.
    let resp = client
        .put("nulltest", serde_json::json!({"v": 42}), None, None)
        .unwrap();
    assert!(resp.ok, "initial put failed: {:?}", resp.error);

    // 2. Verify get returns the value.
    let resp = fetch(&sock, "nulltest.v", None).unwrap().unwrap();
    assert_eq!(
        resp.data.unwrap(),
        serde_json::json!(42),
        "expected v=42 after initial put"
    );

    // 3. Put null: a Put request with data=null clears the cache entry.
    let resp = client
        .put("nulltest", serde_json::Value::Null, None, None)
        .unwrap();
    assert!(resp.ok, "put of null failed: {:?}", resp.error);

    // 4. get should now return miss (no data).
    let data = fetch(&sock, "nulltest.v", None)
        .expect("get after null returned error")
        .and_then(|got| got.data);
    assert!(
        data.is_none(),
        "expected miss after put null, got: {data:?}"
    );

    // 5. A subsequent put under the same key still works (registry survives).
    let resp = client
        .put("nulltest", serde_json::json!({"v": 99}), None, None)
        .unwrap();
    assert!(resp.ok, "second put failed: {:?}", resp.error);

    let resp = fetch(&sock, "nulltest.v", None).unwrap().unwrap();
    assert_eq!(
        resp.data.unwrap(),
        serde_json::json!(99),
        "expected v=99 after second put"
    );

    handle.abort();
}

/// A null put at a node clears only that subtree — sibling records at the same
/// namespace survive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_of_null_at_a_node_clears_only_that_subtree() {
    let (_tmp, sock, client, handle) = setup_daemon().await;

    let resp = client
        .put(
            "nulltree",
            serde_json::json!({"keep": 1, "drop": {"x": 2}}),
            None,
            None,
        )
        .unwrap();
    assert!(resp.ok, "initial put failed: {:?}", resp.error);

    let resp = client
        .put("nulltree.drop", serde_json::Value::Null, None, None)
        .unwrap();
    assert!(resp.ok, "node null put failed: {:?}", resp.error);

    let resp = fetch(&sock, "nulltree", None).expect("get failed").unwrap();
    assert_eq!(
        resp.data.unwrap(),
        serde_json::json!({"keep": 1}),
        "only the addressed subtree is removed"
    );

    handle.abort();
}

/// One raw request line over a fresh connection, the reply parsed.
fn raw(sock: &std::path::Path, request: &serde_json::Value) -> serde_json::Value {
    use std::io::{BufRead, BufReader, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(sock).unwrap();
    writeln!(stream, "{request}").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn got(sock: &std::path::Path, key: &str) -> Option<serde_json::Value> {
    fetch(sock, key, None).unwrap().and_then(|g| g.data)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_data_null_removes_the_node() {
    let (_tmp, sock, client, handle) = setup_daemon().await;
    let r = client
        .put("ns", serde_json::json!({"a": 1}), None, None)
        .unwrap();
    assert!(r.ok);
    assert_eq!(got(&sock, "ns.a"), Some(serde_json::json!(1)));

    let r = raw(
        &sock,
        &serde_json::json!({"op":"put","key":"ns.a","data":null}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(got(&sock, "ns.a"), None);
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_without_data_is_an_invalid_request() {
    let (_tmp, sock, _client, handle) = setup_daemon().await;
    let r = raw(&sock, &serde_json::json!({"op":"put","key":"ns.a"}));
    assert_eq!(r["ok"], false, "{r}");
    let err = r["error"].as_str().unwrap();
    assert!(
        err.contains("invalid request") && err.contains("missing field"),
        "{err}"
    );
    handle.abort();
}

/// A null inside a put's data is absent from what the put states, and the put
/// replaces the value at its address: nothing earlier merges into it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nested_null_is_absent_from_the_replacing_put() {
    let (_tmp, sock, client, handle) = setup_daemon().await;
    client
        .put("ns", serde_json::json!({"a": 1, "b": 2}), None, None)
        .unwrap();
    let r = client
        .put("ns", serde_json::json!({"a": null, "c": 3}), None, None)
        .unwrap();
    assert!(r.ok, "{:?}", r.error);
    assert_eq!(got(&sock, "ns"), Some(serde_json::json!({"c": 3})));

    client
        .put(
            "ns",
            serde_json::json!({"arr": [1, 2, 3], "objs": [{"x": 1}]}),
            None,
            None,
        )
        .unwrap();
    let r = client
        .put(
            "ns",
            serde_json::json!({"arr": [1, null, 3], "objs": [{"x": null}]}),
            None,
            None,
        )
        .unwrap();
    assert!(r.ok, "{:?}", r.error);
    // A null element is absent, and the list closes up behind it.
    assert_eq!(got(&sock, "ns.arr"), Some(serde_json::json!([1, 3])));
    assert_eq!(got(&sock, "ns.objs.0.x"), None);
    assert_eq!(got(&sock, "ns.arr.0"), Some(serde_json::json!(1)));
    assert_eq!(
        got(&sock, "ns.c"),
        None,
        "the second put replaced the first"
    );
    handle.abort();
}

/// `{"o":{"x":null}}` states all of `o`, which is empty: the sibling an earlier
/// put left at `o.y` goes with the rest of the old `o`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_put_whose_only_member_is_null_clears_the_siblings() {
    let (_tmp, sock, client, handle) = setup_daemon().await;
    client
        .put("ns", serde_json::json!({"o": {"y": 1}}), None, None)
        .unwrap();
    client
        .put("ns", serde_json::json!({"o": {"x": null}}), None, None)
        .unwrap();
    assert_eq!(got(&sock, "ns.o.y"), None);
    assert_eq!(got(&sock, "ns.o"), Some(serde_json::json!({})));
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_string_is_a_value() {
    let (_tmp, sock, client, handle) = setup_daemon().await;
    let r = client
        .put("ns.a", serde_json::json!(""), None, None)
        .unwrap();
    assert!(r.ok, "{:?}", r.error);
    assert_eq!(got(&sock, "ns.a"), Some(serde_json::json!("")));
    handle.abort();
}

#[test]
fn comb_put_json_null_removes() {
    use crate::common::daemon::TestDaemon;
    let d = TestDaemon::spawn();
    let cwd = std::path::Path::new("/");
    d.comb(cwd).args(["put", "ns.a", "1"]).assert().success();
    d.comb(cwd).args(["put", "ns.a", "null"]).assert().success();
    d.comb(cwd)
        .args(["get", "-f", "json", "ns.a"])
        .assert()
        .failure();
}

#[test]
fn comb_put_json_null_with_ttl_exits_2() {
    use crate::common::daemon::TestDaemon;
    let d = TestDaemon::spawn();
    let cwd = std::path::Path::new("/");
    d.comb(cwd).args(["put", "ns.a", "1"]).assert().success();
    d.comb(cwd)
        .args(["put", "ns.a", "null", "--ttl", "30s"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "a ttl on an absent value has no meaning",
        ));
    d.comb(cwd)
        .args(["get", "-f", "json", "ns.a"])
        .assert()
        .success();
}

#[test]
fn comb_put_dash_dash_null_is_an_unknown_argument() {
    use crate::common::daemon::TestDaemon;
    let d = TestDaemon::spawn();
    let cwd = std::path::Path::new("/");
    d.comb(cwd).args(["put", "ns.a", "1"]).assert().success();
    d.comb(cwd)
        .args(["put", "ns.a", "--null"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("unexpected argument '--null'"));
    d.comb(cwd)
        .args(["get", "-f", "json", "ns.a"])
        .assert()
        .success();
}

#[test]
fn comb_put_without_data_is_a_usage_error() {
    use crate::common::daemon::TestDaemon;
    let d = TestDaemon::spawn();
    let cwd = std::path::Path::new("/");
    d.comb(cwd)
        .args(["put", "ns.a"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("<DATA>"));
}
