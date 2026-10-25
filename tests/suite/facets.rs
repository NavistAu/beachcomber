//! Facet key syntax (`provider:facet`), checked at the request boundary in
//! `handle_request`/`handle_watch` via `query::parse_facet`. See
//! `docs/canon/field_resolution.md` "Facet" and
//! `docs/canon/provider_source.md` "Cache entry" for the canon this pins.

use crate::common::git::{GitRepoFixture, has_git};
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

// (a) + (b): a facet key (`provider:facet`) round-trips through `put`/`get`
// exactly like any other static-namespace key — the colon is just part of the
// provider name.

#[tokio::test]
async fn put_then_get_field_on_facet_key() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let put = send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp:notes","data":{"x":1}}"#,
    )
    .await;
    assert!(put.ok, "put to a facet key should succeed: {:?}", put.error);

    let get = send_recv(&mut stream, r#"{"op":"get","key":"myapp:notes.x"}"#).await;
    assert!(
        get.ok,
        "get of a facet field should succeed: {:?}",
        get.error
    );
    assert_eq!(get.data.unwrap(), 1);

    handle.abort();
}

#[tokio::test]
async fn get_whole_facet_key_returns_object() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp:notes","data":{"x":1}}"#,
    )
    .await;

    let get = send_recv(&mut stream, r#"{"op":"get","key":"myapp:notes"}"#).await;
    assert!(
        get.ok,
        "get of the whole facet should succeed: {:?}",
        get.error
    );
    assert_eq!(get.data.unwrap(), serde_json::json!({"x": 1}));

    handle.abort();
}

// (c), (d), (e): malformed facet syntax is rejected with an exact
// "invalid facet key: <provider>" error before any provider lookup happens.

#[tokio::test]
async fn get_two_colons_is_invalid_facet() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(&mut stream, r#"{"op":"get","key":"a:b:c.x"}"#).await;
    assert!(!resp.ok, "two colons should be rejected as a facet key");
    assert_eq!(
        resp.error.as_deref(),
        Some("invalid facet key: a:b:c"),
        "got: {:?}",
        resp.error
    );

    handle.abort();
}

#[tokio::test]
async fn get_empty_facet_name_is_invalid() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(&mut stream, r#"{"op":"get","key":"a:.x"}"#).await;
    assert!(!resp.ok, "an empty facet name should be rejected");
    assert_eq!(
        resp.error.as_deref(),
        Some("invalid facet key: a:"),
        "got: {:?}",
        resp.error
    );

    handle.abort();
}

#[tokio::test]
async fn get_facet_name_with_bad_chars_is_invalid() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(&mut stream, r#"{"op":"get","key":"a:b!.x"}"#).await;
    assert!(
        !resp.ok,
        "a facet name with invalid characters should be rejected"
    );
    assert_eq!(
        resp.error.as_deref(),
        Some("invalid facet key: a:b!"),
        "got: {:?}",
        resp.error
    );

    handle.abort();
}

// `watch` checks facet syntax before it ever subscribes — the malformed key
// never reaches the subscription/initial-snapshot machinery.

#[tokio::test]
async fn watch_two_colons_is_invalid_facet_before_subscribing() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    stream
        .write_all(b"{\"op\":\"watch\",\"key\":\"a:b:c.x\"}\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let resp: Response = serde_json::from_str(line.trim()).unwrap();

    assert!(!resp.ok, "watch on an invalid facet key should error");
    assert_eq!(
        resp.error.as_deref(),
        Some("invalid facet key: a:b:c"),
        "got: {:?}",
        resp.error
    );

    handle.abort();
}

// `refresh` shares the same two rules as `get`: facet syntax is checked (and
// rejected exactly the same way), and a metadata suffix is stripped before
// the facet check ever sees the provider segment.

#[tokio::test]
async fn refresh_two_colons_is_invalid_facet() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(&mut stream, r#"{"op":"refresh","key":"a:b:c"}"#).await;
    assert!(!resp.ok, "refresh on an invalid facet key should error");
    assert_eq!(
        resp.error.as_deref(),
        Some("invalid facet key: a:b:c"),
        "got: {:?}",
        resp.error
    );

    handle.abort();
}

#[tokio::test]
async fn refresh_on_a_facet_key_is_an_error() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(
        &mut stream,
        r#"{"op":"put","key":"x:template","data":{"a":"{{ 1 }}"}}"#,
    )
    .await;
    assert!(resp.ok, "put at the facet: {:?}", resp.error);

    // The facet key really is registered — an unregistered one would give the
    // same "no sources" message below and prove nothing about facets.
    let resp = send_recv(&mut stream, r#"{"op":"get","key":"x:template.a"}"#).await;
    assert_eq!(
        resp.data,
        Some(serde_json::json!("{{ 1 }}")),
        "the put must have landed: {:?}",
        resp.error
    );

    // A facet holds static records only. Reading configuration afresh is the
    // config watcher's job, not a refresh's.
    let resp = send_recv(&mut stream, r#"{"op":"refresh","key":"x:template"}"#).await;
    assert!(!resp.ok, "refresh on a facet key should error");
    assert_eq!(
        resp.error.as_deref(),
        Some("nothing to refresh: x:template has no sources"),
        "got: {:?}",
        resp.error
    );

    handle.abort();
}

// One colon form names a facet: a colon in the head (`p:facet`). Below the
// head a colon is part of a segment, like any other character but `.`, `[`
// and `]`.

#[tokio::test]
async fn a_colon_below_the_head_is_part_of_a_segment() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let r = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "git.branch:age"}).to_string(),
    )
    .await;
    assert!(r.ok && r.data.is_none(), "an ordinary miss: {r:?}");

    let key = "scanline.widgets.foo:fresh";
    let r = send_recv(
        &mut stream,
        &serde_json::json!({"op": "put", "key": key, "data": {"x": 1}}).to_string(),
    )
    .await;
    assert!(r.ok, "put {key}: {r:?}");
    let r = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": key}).to_string(),
    )
    .await;
    assert_eq!(r.data, Some(serde_json::json!({"x": 1})), "get {key}");
    let r = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "scanline['widgets']['foo:fresh'].x"}).to_string(),
    )
    .await;
    assert_eq!(r.data, Some(serde_json::json!(1)), "the bracketed spelling");

    handle.abort();
}

#[tokio::test]
async fn a_key_the_grammar_refuses_is_invalid_in_every_op() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    for key in ["p..a", "p['a", "p.a]", "/tmp/x", "~/ws"] {
        let want = Some(format!("invalid key: {key}"));
        for op in ["get", "refresh", "watch"] {
            // A watch ends its connection, so each op gets its own.
            let mut stream = UnixStream::connect(&sock).await.unwrap();
            let r = send_recv(
                &mut stream,
                &serde_json::json!({"op": op, "key": key}).to_string(),
            )
            .await;
            assert!(!r.ok, "{op} {key} must be refused: {r:?}");
            assert_eq!(r.error, want, "{op} {key}");
        }
        let mut stream = UnixStream::connect(&sock).await.unwrap();
        let r = send_recv(
            &mut stream,
            &serde_json::json!({"op": "put", "key": key, "data": {"x": 1}}).to_string(),
        )
        .await;
        assert!(!r.ok, "put {key} must be refused: {r:?}");
        assert_eq!(r.error, want, "put {key}");
    }

    handle.abort();
}

// (h): a facet key's `put`-written row shows up in `status` as a static row —
// source "put", kind "static" — exactly like any other static-namespace entry.

#[tokio::test]
async fn status_row_for_facet_key_put_is_static() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let put = send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp:notes","data":{"x":1}}"#,
    )
    .await;
    assert!(put.ok, "put should succeed: {:?}", put.error);

    let status = send_recv(&mut stream, r#"{"op":"status"}"#).await;
    assert!(status.ok, "status should succeed: {:?}", status.error);
    let rows = status.data.expect("status must return row data");
    let rows = rows.as_array().expect("status data must be an array");

    let row = rows
        .iter()
        .find(|r| r["provider"] == "myapp:notes")
        .expect("a status row for the facet key must be present");
    assert_eq!(row["source"], "put", "row source must be 'put', got {row}");
    assert_eq!(
        row["kind"]["kind"], "static",
        "row kind must be 'static', got {row}"
    );

    handle.abort();
}

// The `:put` and `:source` facets read one layer of a namespace each.

/// `git.branch` is a Source value ("main") once warmed by a plain get; the
/// put at the same address hides it from a plain read only.
async fn warmed_repo_with_a_put_branch(stream: &mut UnixStream, repo: &GitRepoFixture) {
    let p = repo.path_str();
    let warm = send_recv(
        stream,
        &serde_json::json!({"op":"get","key":"git.branch","path":p}).to_string(),
    )
    .await;
    assert_eq!(warm.data.unwrap(), "main");
    let put = send_recv(
        stream,
        &serde_json::json!({"op":"put","key":"git.branch","data":"put-branch","path":p})
            .to_string(),
    )
    .await;
    assert!(put.ok, "{:?}", put.error);
}

#[tokio::test]
async fn the_put_facet_reads_only_the_put_layer() {
    if !has_git() {
        return;
    }
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let repo = GitRepoFixture::new();
    let p = repo.path_str();

    // Before any put, the put layer is empty although the Source answers.
    let before = send_recv(
        &mut stream,
        &serde_json::json!({"op":"get","key":"git:put.branch","path":p}).to_string(),
    )
    .await;
    assert!(before.data.is_none(), "{before:?}");

    warmed_repo_with_a_put_branch(&mut stream, &repo).await;

    let put = send_recv(
        &mut stream,
        &serde_json::json!({"op":"get","key":"git:put.branch","path":p}).to_string(),
    )
    .await;
    assert!(put.ok, "{:?}", put.error);
    assert_eq!(put.data.unwrap(), "put-branch");
    assert!(put.template.is_none(), "a layer facet has no template half");

    let whole = send_recv(
        &mut stream,
        &serde_json::json!({"op":"get","key":"git:put","path":p}).to_string(),
    )
    .await;
    assert_eq!(
        whole.data.unwrap(),
        serde_json::json!({"branch": "put-branch"})
    );

    handle.abort();
}

#[tokio::test]
async fn the_source_facet_shows_a_value_a_put_hides() {
    if !has_git() {
        return;
    }
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let repo = GitRepoFixture::new();
    let p = repo.path_str();

    warmed_repo_with_a_put_branch(&mut stream, &repo).await;

    let plain = send_recv(
        &mut stream,
        &serde_json::json!({"op":"get","key":"git.branch","path":p}).to_string(),
    )
    .await;
    assert_eq!(plain.data.unwrap(), "put-branch");
    let source = send_recv(
        &mut stream,
        &serde_json::json!({"op":"get","key":"git:source.branch","path":p}).to_string(),
    )
    .await;
    assert!(source.ok, "{:?}", source.error);
    assert_eq!(source.data.unwrap(), "main");
    assert!(source.template.is_none());

    handle.abort();
}

#[tokio::test]
async fn the_source_facet_warms_like_the_plain_key() {
    if !has_git() {
        return;
    }
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let repo = GitRepoFixture::new();

    let r = send_recv(
        &mut stream,
        &serde_json::json!({"op":"get","key":"git:source.branch","path":repo.path_str()})
            .to_string(),
    )
    .await;
    assert!(r.ok, "{:?}", r.error);
    assert_eq!(r.data.unwrap(), "main");

    handle.abort();
}

#[tokio::test]
async fn a_put_to_the_put_or_source_facet_is_refused() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    for key in ["p:put", "p:put.x", "p:source", "p:source.x"] {
        let r = send_recv(
            &mut stream,
            &serde_json::json!({"op":"put","key":key,"data":1}).to_string(),
        )
        .await;
        assert!(!r.ok, "put {key} must be refused");
        assert_eq!(
            r.error.as_deref(),
            Some("p:put and p:source are read-only; write p.<node>")
        );
    }

    // The message names the real base namespace, not a hard-coded one.
    let r = send_recv(
        &mut stream,
        r#"{"op":"put","key":"git:put.branch","data":1}"#,
    )
    .await;
    assert_eq!(
        r.error.as_deref(),
        Some("git:put and git:source are read-only; write git.<node>")
    );

    handle.abort();
}

#[tokio::test]
async fn a_refresh_of_a_layer_facet_names_the_key_as_given() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    for key in ["p:put", "p:source", "git:put.branch", "git:source.branch"] {
        let r = send_recv(
            &mut stream,
            &serde_json::json!({"op":"refresh","key":key}).to_string(),
        )
        .await;
        assert!(!r.ok, "refresh {key} must be refused");
        assert_eq!(
            r.error.as_deref(),
            Some(format!("cannot refresh {key}: a layer facet is read-only").as_str()),
        );
    }

    handle.abort();
}

#[tokio::test]
async fn a_second_colon_after_a_layer_facet_is_an_invalid_facet_key() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    for key in ["p:put:age", "p:source:age"] {
        let r = send_recv(
            &mut stream,
            &serde_json::json!({"op":"get","key":key}).to_string(),
        )
        .await;
        assert!(!r.ok, "get {key} must be refused: {r:?}");
        assert_eq!(r.error, Some(format!("invalid facet key: {key}")));
    }

    handle.abort();
}

#[tokio::test]
async fn a_cold_watch_on_the_put_facet_warms_nothing() {
    if !has_git() {
        return;
    }
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache.clone(), registry, None, watchers);
    let handle = server.spawn().unwrap();
    let repo = GitRepoFixture::new();
    let mut stream = UnixStream::connect(&sock).await.unwrap();
    stream
        .write_all(
            format!(
                "{}\n",
                serde_json::json!({"op":"watch","key":"git:put.branch","path":repo.path_str()})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let first: Response = serde_json::from_str(line.trim()).unwrap();
    assert!(first.ok, "{:?}", first.error);
    assert!(first.data.is_none(), "nothing in the put layer: {first:?}");
    assert!(
        cache.list_rows().iter().all(|r| r.provider != "git"),
        "no Source ran for the watch"
    );

    handle.abort();
}
