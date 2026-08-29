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
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

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

#[tokio::test]
async fn store_rejects_builtin_name() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let req = r#"{"op":"put","key":"git","data":{"branch":"main"}}"#;
    let resp = send_recv(&mut stream, req).await;
    assert!(!resp.ok, "store under builtin name should fail");
    let err = resp.error.unwrap();
    assert!(
        err.contains("builtin") || err.contains("script"),
        "error should mention builtin or script, got: {err}"
    );

    handle.abort();
}

#[tokio::test]
async fn store_replaces_previous_data() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // Store v1
    let v1 = r#"{"op":"put","key":"myapp","data":{"status":"starting","old_field":"yes"}}"#;
    let r = send_recv(&mut stream, v1).await;
    assert!(r.ok);

    // Store v2 with different fields
    let v2 = r#"{"op":"put","key":"myapp","data":{"status":"ready","version":"2.0"}}"#;
    let r = send_recv(&mut stream, v2).await;
    assert!(r.ok);

    // Get should return v2 data only
    let get = r#"{"op":"get","key":"myapp"}"#;
    let resp = send_recv(&mut stream, get).await;
    assert!(resp.ok);
    let data = resp.data.unwrap();
    assert_eq!(data["status"], "ready");
    assert_eq!(data["version"], "2.0");
    // old_field should be absent since we replaced the whole entry
    assert!(data.get("old_field").is_none() || data["old_field"].is_null());

    handle.abort();
}

#[tokio::test]
async fn store_with_ttl_shows_staleness() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

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
async fn refresh_virtual_provider_is_noop() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp","data":{"v":"1"}}"#,
    )
    .await;

    let resp = send_recv(&mut stream, r#"{"op":"refresh","key":"myapp"}"#).await;
    assert!(resp.ok);

    let resp = send_recv(&mut stream, r#"{"op":"get","key":"myapp.v"}"#).await;
    assert_eq!(resp.data.unwrap(), "1");

    handle.abort();
}

#[tokio::test]
async fn store_with_path_scope() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp","data":{"v":"proj-a"},"path":"fake-scope-a"}"#,
    )
    .await;
    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp","data":{"v":"proj-b"},"path":"fake-scope-b"}"#,
    )
    .await;

    let resp = send_recv(
        &mut stream,
        r#"{"op":"get","key":"myapp.v","path":"fake-scope-a"}"#,
    )
    .await;
    assert_eq!(resp.data.unwrap(), "proj-a");

    let resp = send_recv(
        &mut stream,
        r#"{"op":"get","key":"myapp.v","path":"fake-scope-b"}"#,
    )
    .await;
    assert_eq!(resp.data.unwrap(), "proj-b");

    handle.abort();
}

// ── Global-slot fallback for virtual providers ──────────────────────────────
//
// A virtual provider declares no sources and therefore no path expression, so
// `docs/canon/field_resolution.md` §"Path resolution" has `get` read it from
// the requested path's slot if that holds an entry and the pathless
// `(provider, None)` one otherwise. (Invariant 2 is the narrower claim about an
// empty/falsy path *expression*; a virtual provider has none to evaluate.)
// A `get` that carries a path (explicitly
// or via connection context) must still find it — otherwise data stored by a
// pathless `put` is unreachable to every caller that supplies a cwd, which is
// what the client SDKs' bc_eval/bc_resolve do unconditionally. `put --path`
// still wins at its own path.
//
// The fallback is slot-level: one slot answers the whole read, never a merge
// of two (`virtual_slots_do_not_merge`).
//
// The mirror-image case — a *non-virtual* PathScoped provider must NOT gain
// this fallback — lives in tests/connection_context.rs, which already owns a
// fake path-scoped provider.

#[tokio::test]
async fn global_put_is_visible_to_a_path_scoped_get() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp","data":{"v":"global-val"}}"#,
    )
    .await;

    // Field read at a path with nothing stored there.
    let resp = send_recv(
        &mut stream,
        r#"{"op":"get","key":"myapp.v","path":"fake-scope-a"}"#,
    )
    .await;
    assert!(resp.ok, "get should succeed: {:?}", resp.error);
    assert_eq!(resp.data.unwrap(), "global-val");

    // Whole-provider read takes the same fallback.
    let resp = send_recv(
        &mut stream,
        r#"{"op":"get","key":"myapp","path":"fake-scope-a"}"#,
    )
    .await;
    assert!(resp.ok, "get should succeed: {:?}", resp.error);
    assert_eq!(resp.data.unwrap()["v"], "global-val");

    handle.abort();
}

#[tokio::test]
async fn path_scoped_put_wins_over_global_at_its_own_path() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp","data":{"v":"global-val"}}"#,
    )
    .await;
    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp","data":{"v":"proj-a"},"path":"fake-scope-a"}"#,
    )
    .await;

    // The path-keyed slot exists, so it answers — no fallback.
    let resp = send_recv(
        &mut stream,
        r#"{"op":"get","key":"myapp.v","path":"fake-scope-a"}"#,
    )
    .await;
    assert_eq!(resp.data.unwrap(), "proj-a");

    // A path with no slot of its own still falls back to the global value.
    let resp = send_recv(
        &mut stream,
        r#"{"op":"get","key":"myapp.v","path":"fake-scope-b"}"#,
    )
    .await;
    assert_eq!(resp.data.unwrap(), "global-val");

    handle.abort();
}

/// Passes with or without the fallback — nothing is stored globally, so there
/// is nothing to fall back to. It guards the *other* direction: that a
/// path-scoped read of an empty slot still reports a miss rather than
/// borrowing a sibling path's value.
#[tokio::test]
async fn path_scoped_get_misses_when_no_global_entry_exists() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp","data":{"v":"proj-a"},"path":"fake-scope-a"}"#,
    )
    .await;

    // Nothing at fake-scope-b and nothing global: a miss, not another path's
    // value and not an error.
    let resp = send_recv(
        &mut stream,
        r#"{"op":"get","key":"myapp.v","path":"fake-scope-b"}"#,
    )
    .await;
    assert!(resp.ok, "miss is ok=true: {:?}", resp.error);
    assert!(resp.data.is_none(), "expected a miss, got {:?}", resp.data);

    handle.abort();
}

/// The fallback picks one slot; it never merges two. A path slot holding `{x}`
/// and a global slot holding `{y}` means `get p.y --path A` misses: the path
/// slot exists, so it answers alone, and it has no `y`.
///
/// This is the intended coherent-snapshot semantics — a read never returns a
/// value stitched together from two independently-written slots — and the
/// price is that `y` is shadowed at A. Documented in canon §"Path resolution".
#[tokio::test]
async fn virtual_slots_do_not_merge() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp","data":{"y":"global-only"}}"#,
    )
    .await;
    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp","data":{"x":"at-a"},"path":"fake-scope-a"}"#,
    )
    .await;

    // The path slot answers, so `x` resolves there.
    let resp = send_recv(
        &mut stream,
        r#"{"op":"get","key":"myapp.x","path":"fake-scope-a"}"#,
    )
    .await;
    assert_eq!(resp.data.unwrap(), "at-a");

    // ...and `y` is shadowed, not merged in from the global slot.
    let resp = send_recv(
        &mut stream,
        r#"{"op":"get","key":"myapp.y","path":"fake-scope-a"}"#,
    )
    .await;
    assert!(resp.ok, "miss is ok=true: {:?}", resp.error);
    assert!(
        resp.data.is_none(),
        "the global slot must not merge into the path slot, got {:?}",
        resp.data
    );

    handle.abort();
}

/// The scenario actually reported: a pathless `put`, then a `context` op
/// setting the connection's path — the CLI's session path, and what every SDK
/// does via bc_eval/bc_resolve. The context path must not hide the global
/// entry any more than an explicit `--path` does.
#[tokio::test]
async fn global_put_is_visible_through_a_connection_context() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp","data":{"v":"global-val"}}"#,
    )
    .await;

    let resp = send_recv(&mut stream, r#"{"op":"context","path":"fake-scope-a"}"#).await;
    assert!(resp.ok, "context should succeed: {:?}", resp.error);

    let resp = send_recv(&mut stream, r#"{"op":"get","key":"myapp.v"}"#).await;
    assert!(resp.ok, "get should succeed: {:?}", resp.error);
    assert_eq!(resp.data.unwrap(), "global-val");

    handle.abort();
}

/// `watch` deliberately keeps no fallback. Its initial read is paired with a
/// `watchers.subscribe` keyed by the same path, so a fallback that only moved
/// the read would emit the global value once and then never update when that
/// value changed — a first value that silently goes stale is worse than a
/// clean miss. Re-keying the subscription alongside the read is the follow-up
/// (docs/roadmap.md).
#[tokio::test]
async fn watch_does_not_take_the_global_fallback() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp","data":{"v":"global-val"}}"#,
    )
    .await;

    // `get` at this path finds the global entry; `watch` at the same path does
    // not — the asymmetry this test exists to pin.
    let resp = send_recv(
        &mut stream,
        r#"{"op":"get","key":"myapp.v","path":"fake-scope-a"}"#,
    )
    .await;
    assert_eq!(resp.data.unwrap(), "global-val");

    let resp = send_recv(
        &mut stream,
        r#"{"op":"watch","key":"myapp.v","path":"fake-scope-a"}"#,
    )
    .await;
    assert!(resp.ok, "watch should open: {:?}", resp.error);
    assert!(
        resp.data.is_none(),
        "watch's initial read takes no global fallback, got {:?}",
        resp.data
    );

    handle.abort();
}
