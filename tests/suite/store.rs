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
    let registry = Arc::new(ProviderRegistry::with_defaults());
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

/// A builtin provider's name is no longer refused wholesale — ownership is
/// checked per record. `git.branch` belongs to the `head` Source, so writing it
/// is refused by that record's name.
#[tokio::test]
async fn store_rejects_a_source_owned_record() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let req = r#"{"op":"put","key":"git","data":{"branch":"main"}}"#;
    let resp = send_recv(&mut stream, req).await;
    assert!(!resp.ok, "storing over a Source-owned record should fail");
    assert_eq!(
        resp.error.as_deref(),
        Some("cannot write git.branch: owned by source head")
    );

    handle.abort();
}

/// A second `put` merges into the first instead of replacing it. `--replace`
/// (`"replace":true` on the wire) is how the old replace-everything behaviour
/// is asked for now.
#[tokio::test]
async fn store_merges_into_previous_data() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let v1 = r#"{"op":"put","key":"myapp","data":{"status":"starting","old_field":"yes"}}"#;
    let r = send_recv(&mut stream, v1).await;
    assert!(r.ok);

    let v2 = r#"{"op":"put","key":"myapp","data":{"status":"ready","version":"2.0"}}"#;
    let r = send_recv(&mut stream, v2).await;
    assert!(r.ok);

    let get = r#"{"op":"get","key":"myapp"}"#;
    let resp = send_recv(&mut stream, get).await;
    assert!(resp.ok);
    let data = resp.data.unwrap();
    assert_eq!(data["status"], "ready", "the second put wins per record");
    assert_eq!(data["version"], "2.0");
    assert_eq!(data["old_field"], "yes", "untouched records survive");

    // The same write with replace:true drops everything under the root first.
    let v3 = r#"{"op":"put","key":"myapp","data":{"status":"done"},"replace":true}"#;
    let r = send_recv(&mut stream, v3).await;
    assert!(r.ok);
    let resp = send_recv(&mut stream, get).await;
    assert_eq!(resp.data.unwrap(), serde_json::json!({"status": "done"}));

    handle.abort();
}

#[tokio::test]
async fn store_with_ttl_shows_staleness() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // Store with TTL of 1 second
    let req = r#"{"op":"put","key":"myapp","data":{"status":"ok"},"ttl":"1s"}"#;
    let r = send_recv(&mut stream, req).await;
    assert!(r.ok);

    // Immediately get — should not be stale
    let get = r#"{"op":"get","key":"myapp"}"#;
    let resp = send_recv(&mut stream, get).await;
    assert!(resp.ok);
    assert_eq!(
        resp.stale,
        Some(false),
        "should not be stale immediately after store"
    );

    // Advance mock clock by 3 seconds past the 1s TTL.
    // is_stale() checks elapsed().as_secs() > interval; with ttl=1, need elapsed >= 2s.
    // Hybrid: pause tokio clock, advance past TTL, then resume so subsequent I/O works normally.
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(3)).await;
    tokio::time::resume();

    let resp2 = send_recv(&mut stream, get).await;
    assert!(resp2.ok);
    assert_eq!(resp2.stale, Some(true), "should be stale after TTL expires");

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
// every SDK's bc_eval/bc_resolve needs, since they supply a cwd unconditionally.
//
// This is layering, not a fallback: the chain is always merged, so a nearer
// record shadows a farther one record by record rather than one whole entry
// answering alone.
//
// The chain's other halves — `$HOME` as the global path, provider records
// staying at their Source's declared scope, and `watch` following the chain —
// live in tests/path_chain.rs.

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

/// A `context` op sets the connection's path — the SDKs' bc_eval/bc_resolve
/// path. It reaches the chain exactly as an explicit `--path` does.
#[tokio::test]
async fn global_put_is_visible_through_a_connection_context() {
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

    let resp = send_recv(&mut stream, &format!(r#"{{"op":"context","path":"{a}"}}"#)).await;
    assert!(resp.ok, "context should succeed: {:?}", resp.error);

    let resp = send_recv(&mut stream, r#"{"op":"get","key":"myapp.v"}"#).await;
    assert!(resp.ok, "get should succeed: {:?}", resp.error);
    assert_eq!(resp.data.unwrap(), "global-val");

    handle.abort();
}

/// `watch` subscribes to every entry on the chain, so its initial frame is the
/// same chain-resolved value `get` returns. (The streaming half — a change to a
/// farther entry waking a watch at a nearer path — lives in
/// tests/path_chain.rs.)
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
