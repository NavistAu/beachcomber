//! Facet keys (`provider:facet`), checked at the request boundary by
//! `libbeachcomber::key::split_facet`: the facets are `template`, `macro`,
//! `memo`, `put`, `source` and `cache`, and no other.

use crate::common::git::GitRepoFixture;
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

/// The daemon's words for a head whose facet is not one of the six.
fn invalid_facet(head: &str) -> String {
    format!("invalid facet key: {head} (a facet is template, macro, memo, put, source or cache)")
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

// (a) + (b): the template facet round-trips through `put`/`get`, read back as
// it was stored: a facet key has no template half of its own.

#[tokio::test]
async fn put_then_get_field_on_facet_key() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let put = send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp:template","data":{"x":"{{ 1 }}"}}"#,
    )
    .await;
    assert!(put.ok, "put to a facet key should succeed: {:?}", put.error);

    let get = send_recv(&mut stream, r#"{"op":"get","key":"myapp:template.x"}"#).await;
    assert!(
        get.ok,
        "get of a facet field should succeed: {:?}",
        get.error
    );
    assert_eq!(get.data.unwrap(), "{{ 1 }}");

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
        r#"{"op":"put","key":"myapp:template","data":{"x":"{{ 1 }}"}}"#,
    )
    .await;

    let get = send_recv(&mut stream, r#"{"op":"get","key":"myapp:template"}"#).await;
    assert!(
        get.ok,
        "get of the whole facet should succeed: {:?}",
        get.error
    );
    assert_eq!(get.data.unwrap(), serde_json::json!({"x": "{{ 1 }}"}));

    handle.abort();
}

/// `<p>:macro` holds `<p>`'s macro libraries: read whole or one library at a
/// time, with no template half, the nearest directory's winning.
#[tokio::test]
async fn macro_is_a_facet() {
    let (tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let rate = "{% macro row() %}r{% endmacro %}";
    let usage = "{% macro usage() %}u{% endmacro %}";
    let mine = "{% macro row() %}mine{% endmacro %}";

    let put = send_recv(
        &mut stream,
        &serde_json::json!({"op": "put", "key": "scanline:macro", "data": {"rate": rate, "usage": usage}})
            .to_string(),
    )
    .await;
    assert!(put.ok, "{:?}", put.error);
    let whole = send_recv(&mut stream, r#"{"op":"get","key":"scanline:macro"}"#).await;
    assert_eq!(
        whole.data,
        Some(serde_json::json!({"rate": rate, "usage": usage}))
    );
    assert_eq!(whole.template, None, "a library is no template");
    let one = send_recv(&mut stream, r#"{"op":"get","key":"scanline:macro.rate"}"#).await;
    assert_eq!(one.data, Some(serde_json::json!(rate)));

    // A put at a directory shadows the global library there, and only there.
    let dir = tmp.path().canonicalize().unwrap().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.to_str().unwrap();
    let put = send_recv(
        &mut stream,
        &serde_json::json!({"op": "put", "key": "scanline:macro.rate", "data": mine, "path": dir})
            .to_string(),
    )
    .await;
    assert!(put.ok, "{:?}", put.error);
    let near = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "scanline:macro", "path": dir}).to_string(),
    )
    .await;
    assert_eq!(
        near.data,
        Some(serde_json::json!({"rate": mine, "usage": usage}))
    );
    let global = send_recv(&mut stream, r#"{"op":"get","key":"scanline:macro.rate"}"#).await;
    assert_eq!(global.data, Some(serde_json::json!(rate)));

    handle.abort();
}

/// A put to `<p>:macro` is checked as a configured library is: a string,
/// under a name, at `<p>:macro.<name>` or as a table of them, that compiles
/// and prints nothing. A refused put writes nothing; a library importing one
/// nothing declares yet is taken.
#[tokio::test]
async fn a_put_to_macro_is_linted() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let prints = "a macro library prints nothing: its text, {{ }} and {% call %} belong inside \
                  a {% macro %}";
    for (key, data, error) in [
        ("p:macro.bad", serde_json::json!("{% macro m( %}"), None),
        (
            "p:macro.text",
            serde_json::json!("hello"),
            Some(format!("p:macro.text: {prints}")),
        ),
        (
            "p:macro",
            serde_json::json!({"t": "{{ 1 }}"}),
            Some(format!("p:macro.t: {prints}")),
        ),
        (
            "p:macro.inc",
            serde_json::json!("{% include 'p:macro.x' %}"),
            Some("p:macro.inc: {% include %} is refused: text is reused with a macro".into()),
        ),
        (
            "p:macro.n",
            serde_json::json!(1),
            Some("p:macro.n must be a string (a macro library)".into()),
        ),
        (
            "p:macro",
            serde_json::json!({"x": {"y": "z"}}),
            Some("p:macro.x must be a string (a macro library)".into()),
        ),
        (
            "p:macro",
            serde_json::json!("x"),
            Some(
                "p:macro: a put writes a macro library at p:macro.<name>, or a table of them"
                    .into(),
            ),
        ),
        (
            "p:macro.a.b",
            serde_json::json!("x"),
            Some("p:macro.a.b: a macro library is p:macro.<name>".into()),
        ),
    ] {
        let r = send_recv(
            &mut stream,
            &serde_json::json!({"op": "put", "key": key, "data": data}).to_string(),
        )
        .await;
        assert!(!r.ok, "{key} {data} is refused");
        let got = r.error.unwrap_or_default();
        match error {
            Some(error) => assert_eq!(got, error, "{key} {data}"),
            None => assert!(got.starts_with("p:macro.bad: "), "{got}"),
        }
    }
    let r = send_recv(&mut stream, r#"{"op":"get","key":"p:macro"}"#).await;
    assert_eq!(r.data, None, "nothing was written");

    let r = send_recv(
        &mut stream,
        &serde_json::json!({"op": "put", "key": "p:macro.ok", "data":
            "{% from 'q:macro.later' import m %}{% macro ok() %}{{ m() }}{% endmacro %}"})
        .to_string(),
    )
    .await;
    assert!(
        r.ok,
        "a library no layer declares yet is taken: {:?}",
        r.error
    );

    handle.abort();
}

// The facets are a closed set: `template`, `macro`, `memo`, `put`, `source`
// and `cache`. Any other name after the colon is refused by every op that takes a
// key, naming the head as the caller wrote it, and nothing is stored or read.

#[tokio::test]
async fn an_unknown_facet_is_an_invalid_facet_key_on_every_op() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache.clone(), registry, None, watchers);
    let handle = server.spawn().unwrap();

    for (key, head) in [
        ("myapp:notes", "myapp:notes"),
        ("myapp:notes.x", "myapp:notes"),
        ("git:memos.branch", "git:memos"),
        ("p:Template", "p:Template"),
        ("p:bogus:put.x", "p:bogus:put"),
        ("p:bogus:cache", "p:bogus:cache"),
    ] {
        let want = Some(invalid_facet(head));
        for op in ["get", "refresh", "watch", "layers"] {
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
    assert!(
        cache.list_rows().is_empty(),
        "nothing was stored: {:?}",
        cache.list_rows()
    );

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
        Some(invalid_facet("a:b:c").as_str()),
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
        Some(invalid_facet("a:").as_str()),
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
        Some(invalid_facet("a:b!").as_str()),
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
        Some(invalid_facet("a:b:c").as_str()),
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
        Some(invalid_facet("a:b:c").as_str()),
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
        r#"{"op":"put","key":"myapp:template","data":{"x":"{{ 1 }}"}}"#,
    )
    .await;
    assert!(put.ok, "put should succeed: {:?}", put.error);

    let status = send_recv(&mut stream, r#"{"op":"status"}"#).await;
    assert!(status.ok, "status should succeed: {:?}", status.error);
    let rows = status.data.expect("status must return row data");
    let rows = rows.as_array().expect("status data must be an array");

    let row = rows
        .iter()
        .find(|r| r["provider"] == "myapp:template")
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
        assert_eq!(r.error, Some(invalid_facet(key)));
    }

    handle.abort();
}

#[tokio::test]
async fn a_cold_watch_on_the_put_facet_warms_nothing() {
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
