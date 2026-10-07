use beachcomber::cache::{CacheRow, RowKind};
use beachcomber::client::Client;
use beachcomber::config::Config;

async fn setup_daemon() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    Client,
    tokio::task::JoinHandle<()>,
) {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let (handle, ready) =
        beachcomber::daemon::start_in_process_polling(sock.clone(), Config::default());
    ready.await.expect("daemon ready");
    let client = Client::new(sock.clone());
    (tmp, sock, client, handle)
}

fn raw(sock: &std::path::Path, request: &serde_json::Value) -> serde_json::Value {
    use std::io::{BufRead, BufReader, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(sock).unwrap();
    writeln!(stream, "{request}").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn put_rows(client: &Client, provider: &str) -> Vec<CacheRow> {
    let mut rows: Vec<CacheRow> = client
        .status()
        .unwrap()
        .into_iter()
        .filter(|r| r.provider == provider && r.source == "put")
        .collect();
    rows.sort_by(|a, b| a.field.cmp(&b.field));
    rows
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_put_address_gets_its_own_lifecycle() {
    let (_tmp, _sock, client, handle) = setup_daemon().await;
    client
        .put(
            "build.status",
            serde_json::json!("passing"),
            Some("300s"),
            None,
        )
        .unwrap();
    client
        .put("build.eta", serde_json::json!("4m"), Some("60s"), None)
        .unwrap();

    let rows = put_rows(&client, "build");
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].field, "eta");
    assert_eq!(rows[0].poll_interval_secs, Some(60));
    assert_eq!(rows[1].field, "status");
    assert_eq!(rows[1].poll_interval_secs, Some(300));
    for row in &rows {
        assert_eq!(row.poll_count, Some(1), "{row:?}");
        assert!(
            matches!(row.kind, Some(RowKind::Lifecycle { decay: 0, .. })),
            "{row:?}"
        );
    }
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nested_row_takes_the_lifecycle_of_the_put_address_above_it() {
    let (_tmp, _sock, client, handle) = setup_daemon().await;
    client
        .put("ci", serde_json::json!({"x": {"y": 1}}), Some("60s"), None)
        .unwrap();
    let rows = put_rows(&client, "ci");
    assert!(!rows.is_empty());
    for row in &rows {
        assert!(
            matches!(row.kind, Some(RowKind::Lifecycle { .. })),
            "{row:?}"
        );
        assert_eq!(row.poll_interval_secs, Some(60), "{row:?}");
    }
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_put_without_ttl_is_static() {
    let (_tmp, _sock, client, handle) = setup_daemon().await;
    client
        .put("build.status", serde_json::json!("passing"), None, None)
        .unwrap();
    let rows = put_rows(&client, "build");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, Some(RowKind::Static));
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reput_without_ttl_drops_the_lifecycle() {
    let (_tmp, _sock, client, handle) = setup_daemon().await;
    client
        .put(
            "build.status",
            serde_json::json!("passing"),
            Some("300s"),
            None,
        )
        .unwrap();
    assert!(matches!(
        put_rows(&client, "build")[0].kind,
        Some(RowKind::Lifecycle { .. })
    ));
    client
        .put("build.status", serde_json::json!("failing"), None, None)
        .unwrap();
    let rows = put_rows(&client, "build");
    assert_eq!(rows[0].kind, Some(RowKind::Static), "{rows:?}");
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_with_an_invalid_ttl_is_refused() {
    let (_tmp, _sock, client, handle) = setup_daemon().await;
    let r = client
        .put("build.status", serde_json::json!("x"), Some("soon"), None)
        .unwrap();
    assert!(!r.ok);
    assert_eq!(r.error.as_deref(), Some("invalid ttl: soon"));
    let r = client
        .put("build.status", serde_json::json!("x"), Some("0s"), None)
        .unwrap();
    assert!(!r.ok);
    assert_eq!(r.error.as_deref(), Some("ttl must be at least 1s"));
    assert!(put_rows(&client, "build").is_empty(), "nothing was written");
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_null_with_a_ttl_is_refused() {
    let (_tmp, sock, client, handle) = setup_daemon().await;
    client
        .put("build.status", serde_json::json!("passing"), None, None)
        .unwrap();
    let r = raw(
        &sock,
        &serde_json::json!({"op":"put","key":"build.status","data":null,"ttl":"30s"}),
    );
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(r["error"], "a ttl on an absent value has no meaning");
    assert_eq!(
        put_rows(&client, "build").len(),
        1,
        "the value was not removed"
    );
    handle.abort();
}
