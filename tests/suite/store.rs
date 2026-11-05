use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn setup() -> (
    TempDir,
    std::path::PathBuf,
    Arc<Cache>,
    Arc<ProviderRegistry>,
    Arc<beachcomber::watcher_registry::WatcherRegistry>,
) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());
    (tmp, sock, cache, registry, watchers)
}

async fn send_recv(stream: &mut UnixStream, request: &str) -> Response {
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(&mut *stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn store_and_get_roundtrip() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // Store data
    let store_req = r#"{"op":"put","key":"myapp","data":{"status":"healthy","version":"1.2.3"}}"#;
    let store_resp = send_recv(&mut stream, store_req).await;
    assert!(
        store_resp.ok,
        "store should succeed: {:?}",
        store_resp.error
    );

    // Get all fields
    let get_req = r#"{"op":"get","key":"myapp"}"#;
    let get_resp = send_recv(&mut stream, get_req).await;
    assert!(get_resp.ok, "get should succeed: {:?}", get_resp.error);
    let data = get_resp.data.unwrap();
    assert_eq!(data["status"], "healthy");
    assert_eq!(data["version"], "1.2.3");

    // Get single field
    let get_field_req = r#"{"op":"get","key":"myapp.status"}"#;
    let get_field_resp = send_recv(&mut stream, get_field_req).await;
    assert!(get_field_resp.ok);
    assert_eq!(get_field_resp.data.unwrap(), serde_json::json!("healthy"));

    handle.abort();
}

/// A put over a Source's field is accepted and wins: layers, not ownership.
#[tokio::test]
async fn a_put_over_a_source_field_is_accepted_and_wins() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let req = r#"{"op":"put","key":"git","data":{"branch":"x"}}"#;
    let resp = send_recv(&mut stream, req).await;
    assert!(resp.ok, "put over a Source field: {:?}", resp.error);

    let resp = send_recv(&mut stream, r#"{"op":"get","key":"git.branch"}"#).await;
    assert_eq!(resp.data.unwrap(), serde_json::json!("x"));

    handle.abort();
}

#[tokio::test]
async fn refresh_on_a_sourceless_namespace_is_an_error() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(&mut stream, r#"{"op":"put","key":"my","data":{"a":"1"}}"#).await;

    // A static namespace has no Source to re-execute. Answering ok claimed
    // work that never happened.
    let resp = send_recv(&mut stream, r#"{"op":"refresh","key":"my"}"#).await;
    assert!(!resp.ok, "refresh on a static namespace should error");
    assert_eq!(
        resp.error.as_deref(),
        Some("nothing to refresh: my has no sources")
    );

    let resp = send_recv(&mut stream, r#"{"op":"refresh","key":"my.a"}"#).await;
    assert!(!resp.ok, "refresh on a static record should error");
    assert_eq!(
        resp.error.as_deref(),
        Some("nothing to refresh: my.a has no sources")
    );

    // The record is where it was.
    let resp = send_recv(&mut stream, r#"{"op":"get","key":"my.a"}"#).await;
    assert_eq!(resp.data.unwrap(), "1");

    handle.abort();
}

/// Two sibling directories under `tmp`, as real paths so the daemon's
/// canonicalisation and the chain walk both have something to chew on.
fn scopes(tmp: &TempDir) -> (String, String) {
    let root = tmp.path().canonicalize().unwrap();
    let a = root.join("scope-a");
    let b = root.join("scope-b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    (
        a.to_string_lossy().into_owned(),
        b.to_string_lossy().into_owned(),
    )
}

#[tokio::test]
async fn store_with_path_scope() {
    let (tmp, sock, cache, registry, watchers) = setup();
    let (a, b) = scopes(&tmp);
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        &format!(r#"{{"op":"put","key":"myapp","data":{{"v":"proj-a"}},"path":"{a}"}}"#),
    )
    .await;
    send_recv(
        &mut stream,
        &format!(r#"{{"op":"put","key":"myapp","data":{{"v":"proj-b"}},"path":"{b}"}}"#),
    )
    .await;

    let resp = send_recv(
        &mut stream,
        &format!(r#"{{"op":"get","key":"myapp.v","path":"{a}"}}"#),
    )
    .await;
    assert_eq!(resp.data.unwrap(), "proj-a");

    let resp = send_recv(
        &mut stream,
        &format!(r#"{{"op":"get","key":"myapp.v","path":"{b}"}}"#),
    )
    .await;
    assert_eq!(resp.data.unwrap(), "proj-b");

    handle.abort();
}

// ── Static records resolve up the directory chain ───────────────────────────
//
// A read at a path collects the static records — those whose source is `put` or
// `config:<file>` — from every entry on the chain from that path up to `$HOME`,
// the nearest winning (`docs/canon/field_resolution.md` §"Path resolution").
// The pathless entry is the global one, the last link of every chain, so data a
// pathless `put` stored stays reachable from any directory — which is what
// every SDK's bc_get/bc_eval needs, since they supply a cwd unconditionally.
//
// This is layering, not a fallback: the chain is always merged, so a nearer
// record shadows a farther one record by record rather than one whole entry
// answering alone.
//
// The chain's other halves — `$HOME` as the global path, provider records
// staying at their Source's declared scope, and `watch` following the chain —
// live in tests/suite/path_chain.rs.

#[tokio::test]
async fn a_global_put_is_visible_at_every_path() {
    let (tmp, sock, cache, registry, watchers) = setup();
    let (a, _b) = scopes(&tmp);
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp","data":{"v":"global-val"}}"#,
    )
    .await;

    // Field read at a path with nothing stored there.
    let resp = send_recv(
        &mut stream,
        &format!(r#"{{"op":"get","key":"myapp.v","path":"{a}"}}"#),
    )
    .await;
    assert!(resp.ok, "get should succeed: {:?}", resp.error);
    assert_eq!(resp.data.unwrap(), "global-val");

    // Whole-namespace read resolves through the same chain.
    let resp = send_recv(
        &mut stream,
        &format!(r#"{{"op":"get","key":"myapp","path":"{a}"}}"#),
    )
    .await;
    assert!(resp.ok, "get should succeed: {:?}", resp.error);
    assert_eq!(resp.data.unwrap()["v"], "global-val");

    handle.abort();
}

#[tokio::test]
async fn path_scoped_put_wins_over_global_at_its_own_path() {
    let (tmp, sock, cache, registry, watchers) = setup();
    let (a, b) = scopes(&tmp);
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp","data":{"v":"global-val"}}"#,
    )
    .await;
    send_recv(
        &mut stream,
        &format!(r#"{{"op":"put","key":"myapp","data":{{"v":"proj-a"}},"path":"{a}"}}"#),
    )
    .await;

    // The nearer record wins at its own path.
    let resp = send_recv(
        &mut stream,
        &format!(r#"{{"op":"get","key":"myapp.v","path":"{a}"}}"#),
    )
    .await;
    assert_eq!(resp.data.unwrap(), "proj-a");

    // A sibling path has no record of its own, so the global one is what the
    // chain reaches.
    let resp = send_recv(
        &mut stream,
        &format!(r#"{{"op":"get","key":"myapp.v","path":"{b}"}}"#),
    )
    .await;
    assert_eq!(resp.data.unwrap(), "global-val");

    handle.abort();
}

/// A read never borrows a *sibling* path's value: the chain runs upward only.
#[tokio::test]
async fn a_read_misses_when_no_entry_on_its_chain_holds_the_record() {
    let (tmp, sock, cache, registry, watchers) = setup();
    let (a, b) = scopes(&tmp);
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        &format!(r#"{{"op":"put","key":"myapp","data":{{"v":"proj-a"}},"path":"{a}"}}"#),
    )
    .await;

    // Nothing at scope-b, nothing on the way up, and nothing global: a miss,
    // not scope-a's value and not an error.
    let resp = send_recv(
        &mut stream,
        &format!(r#"{{"op":"get","key":"myapp.v","path":"{b}"}}"#),
    )
    .await;
    assert!(resp.ok, "miss is ok=true: {:?}", resp.error);
    assert!(resp.data.is_none(), "expected a miss, got {:?}", resp.data);

    handle.abort();
}

/// The chain merges record by record. A path entry holding `{x}` and the global
/// entry holding `{y}` means a read at that path sees both — `y` is inherited,
/// not shadowed, because nothing nearer claims it.
#[tokio::test]
async fn records_merge_across_the_chain() {
    let (tmp, sock, cache, registry, watchers) = setup();
    let (a, _b) = scopes(&tmp);
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp","data":{"y":"global-only"}}"#,
    )
    .await;
    send_recv(
        &mut stream,
        &format!(r#"{{"op":"put","key":"myapp","data":{{"x":"at-a"}},"path":"{a}"}}"#),
    )
    .await;

    let resp = send_recv(
        &mut stream,
        &format!(r#"{{"op":"get","key":"myapp.x","path":"{a}"}}"#),
    )
    .await;
    assert_eq!(resp.data.unwrap(), "at-a");

    let resp = send_recv(
        &mut stream,
        &format!(r#"{{"op":"get","key":"myapp.y","path":"{a}"}}"#),
    )
    .await;
    assert!(resp.ok, "get should succeed: {:?}", resp.error);
    assert_eq!(
        resp.data.unwrap(),
        "global-only",
        "a record no nearer entry claims is inherited from up the chain"
    );

    // The whole-namespace read is the merged tree, not one entry's map.
    let resp = send_recv(
        &mut stream,
        &format!(r#"{{"op":"get","key":"myapp","path":"{a}"}}"#),
    )
    .await;
    let data = resp.data.unwrap();
    assert_eq!(data["x"], "at-a");
    assert_eq!(data["y"], "global-only");

    handle.abort();
}

/// `watch` subscribes to every entry on the chain, so its initial frame is the
/// same chain-resolved value `get` returns. (The streaming half — a change to a
/// farther entry waking a watch at a nearer path — lives in
/// tests/suite/path_chain.rs.)
#[tokio::test]
async fn watch_reads_through_the_chain() {
    let (tmp, sock, cache, registry, watchers) = setup();
    let (a, _b) = scopes(&tmp);
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp","data":{"v":"global-val"}}"#,
    )
    .await;

    let resp = send_recv(
        &mut stream,
        &format!(r#"{{"op":"get","key":"myapp.v","path":"{a}"}}"#),
    )
    .await;
    assert_eq!(resp.data.unwrap(), "global-val");

    let resp = send_recv(
        &mut stream,
        &format!(r#"{{"op":"watch","key":"myapp.v","path":"{a}"}}"#),
    )
    .await;
    assert!(resp.ok, "watch should open: {:?}", resp.error);
    assert_eq!(
        resp.data.unwrap(),
        "global-val",
        "watch's initial frame resolves through the chain, like get"
    );

    handle.abort();
}
