//! The `:memo` facet and the `memo` op: what a template leaf's `| cache()`
//! stores, how a read finds it (the memo half of a `get`, beside the template
//! half), what `:cache` says of it, and what drops its hash. The client half
//! (the filters, the write-back) is the resolver's; these drive the daemon
//! over its socket as that client would.

use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use beachcomber::watcher_registry::WatcherRegistry;
use serde_json::{Value as Json, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const H1: &str = "9c6001d328a94690";
const H2: &str = "0123456789abcdef";

struct Served {
    _tmp: TempDir,
    sock: PathBuf,
    s: UnixStream,
    handle: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn serve_with(registry: ProviderRegistry) -> Served {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("s");
    let watchers = Arc::new(WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let handle = Server::new(sock.clone(), cache, Arc::new(registry), None, watchers)
        .spawn()
        .unwrap();
    let s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    s.set_nonblocking(true).unwrap();
    Served {
        _tmp: tmp,
        s: UnixStream::from_std(s).unwrap(),
        sock,
        handle,
    }
}

fn serve() -> Served {
    serve_with(crate::common::registry::with_shipped_defaults())
}

impl Served {
    async fn send(&mut self, request: Json) -> Response {
        self.s
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let mut reader = BufReader::new(&mut self.s);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn get(&mut self, key: &str, path: Option<&str>) -> Response {
        self.send(json!({"op": "get", "key": key, "path": path}))
            .await
    }

    async fn put(&mut self, key: &str, data: Json) -> Response {
        self.send(json!({"op": "put", "key": key, "data": data}))
            .await
    }

    /// The write-back a render makes: `data` `None` is a memo of absence.
    async fn memo(
        &mut self,
        key: &str,
        path: Option<&str>,
        hash: &str,
        data: Option<Json>,
        watch: &[&str],
        ttl: Option<&str>,
    ) -> Response {
        let r = self
            .send(json!({"op": "memo", "key": key, "path": path, "hash": hash,
                         "data": data, "watch": watch, "ttl": ttl}))
            .await;
        assert!(r.ok, "memo {key}: {:?}", r.error);
        r
    }

    /// `p:cache.<f>`, the record a read of `p.<f>` resolves to.
    async fn cache_of(&mut self, key: &str, path: Option<&str>) -> Json {
        let (head, tail) = key.split_once('.').unwrap();
        let r = self.get(&format!("{head}:cache.{tail}"), path).await;
        assert!(r.ok, "{:?}", r.error);
        r.data.unwrap_or(Json::Null)
    }
}

fn canonical(dir: &TempDir) -> PathBuf {
    dir.path().canonicalize().unwrap()
}

#[tokio::test]
async fn memo_is_a_facet() {
    let mut d = serve();
    let r = d.get("demo:memo", None).await;
    assert!(r.ok && r.data.is_none(), "nothing written is a miss: {r:?}");

    let r = d.put("demo:memo.x", json!(1)).await;
    assert_eq!(
        r.error.as_deref(),
        Some("demo:memo is written by cache(); edit demo:template.<f>, or refresh demo")
    );

    let r = d.get("demo:bogus", None).await;
    assert_eq!(
        r.error.as_deref(),
        Some(
            "invalid facet key: demo:bogus (a facet is template, macro, memo, put, source or cache)"
        )
    );

    // A refresh of a memo key is refused as a refresh of a facet is.
    let r = d.send(json!({"op": "refresh", "key": "demo:memo.f"})).await;
    assert_eq!(
        r.error.as_deref(),
        Some("nothing to refresh: demo:memo.f has no sources")
    );
}

#[tokio::test]
async fn a_memo_op_stores_a_value_and_its_keys() {
    let mut d = serve();
    assert!(d.put("demo:template.f", json!("{{ 'x' }}")).await.ok);
    d.memo(
        "demo.f",
        None,
        H1,
        Some(json!("prod")),
        &["/h/.kube/config"],
        Some("10m"),
    )
    .await;

    let r = d.get("demo:memo.f", None).await;
    assert_eq!(r.data, Some(json!("prod")));
    assert!(
        r.template.is_none() && r.memo.is_none(),
        "a facet has no halves"
    );
    assert_eq!(
        d.get("demo:memo", None).await.data,
        Some(json!({"f": "prod"}))
    );

    let mut leaf = d.cache_of("demo.f", None).await;
    assert!(leaf["age_ms"].is_u64(), "{leaf}");
    leaf.as_object_mut().unwrap().remove("age_ms");
    assert_eq!(
        leaf,
        json!({"source": "memo", "hash": H1,
               "watch": ["/h/.kube/config"],
               "poll": {"interval": "600s", "count": 1}, "decay": 1})
    );
}

#[tokio::test]
async fn a_memo_of_absence_has_keys_and_no_value() {
    let mut d = serve();
    assert!(d.put("demo:template.f", json!("{{ none }}")).await.ok);
    d.memo("demo.f", None, H1, None, &[], None).await;

    let r = d.get("demo:memo.f", None).await;
    assert!(r.ok && r.data.is_none(), "no value is stored: {r:?}");
    let leaf = d.cache_of("demo.f", None).await;
    assert_eq!(leaf["source"], json!("memo"), "{leaf}");
    assert_eq!(leaf["hash"], json!(H1), "{leaf}");
    for key in ["poll", "decay", "watch"] {
        assert!(
            leaf.get(key).is_none(),
            "{key} without a ttl or a path: {leaf}"
        );
    }
    let half = d.get("demo.f", None).await.memo.unwrap();
    assert_eq!(half["hash"], json!(H1));
    assert!(half.get("data").is_none(), "absent, never null: {half}");
}

#[tokio::test]
async fn a_get_carries_the_memo_half() {
    let mut d = serve();
    assert!(
        d.put(
            "demo:template",
            json!({"f": "{{ 'x' }}", "g": "{{ 'y' }}", "grp": {"h": "{{ 'z' }}"}})
        )
        .await
        .ok
    );
    d.memo("demo.f", None, H1, Some(json!("prod")), &[], None)
        .await;
    d.memo("demo.grp.h", None, H2, Some(json!({"n": 1})), &[], None)
        .await;

    let r = d.get("demo.f", None).await;
    assert_eq!(r.template, Some(json!("{{ 'x' }}")));
    let mut half = r.memo.unwrap();
    assert!(half["age_ms"].is_u64(), "{half}");
    half.as_object_mut().unwrap().remove("age_ms");
    assert_eq!(half, json!({"hash": H1, "stale": false, "data": "prod"}));

    // A group's half is shaped like its template, a leaf with no memo left out.
    let tree = d.get("demo", None).await.memo.unwrap();
    assert_eq!(tree["f"]["data"], json!("prod"), "{tree}");
    assert_eq!(tree["grp"]["h"]["hash"], json!(H2), "{tree}");
    assert_eq!(tree["grp"]["h"]["data"], json!({"n": 1}), "{tree}");
    assert!(tree.get("g").is_none(), "{tree}");

    // A leaf with no memo carries no half.
    assert!(d.get("demo.g", None).await.memo.is_none());
    // The half of a string reached with segments left over is the string's.
    let r = d.get("demo.f.deeper", None).await;
    assert_eq!(r.rest.as_deref(), Some("deeper"));
    assert_eq!(r.memo.unwrap()["hash"], json!(H1));

    // A memo is read only through its template: none, no half.
    d.memo("other.f", None, H1, Some(json!("v")), &[], None)
        .await;
    let r = d.get("other.f", None).await;
    assert!(r.ok && r.memo.is_none() && r.data.is_none(), "{r:?}");
}

#[tokio::test]
async fn the_memo_half_is_found_up_the_chain_nearest_first() {
    let mut d = serve();
    let root = TempDir::new().unwrap();
    let a = canonical(&root).join("a");
    let b = a.join("b");
    std::fs::create_dir_all(&b).unwrap();
    let (a, b) = (a.to_str().unwrap(), b.to_str().unwrap());
    assert!(d.put("demo:template.f", json!("{{ 'x' }}")).await.ok);

    d.memo("demo.f", Some(a), H1, Some(json!("at a")), &[], None)
        .await;
    let half = d.get("demo.f", Some(b)).await.memo.unwrap();
    assert_eq!(half["data"], json!("at a"), "shared down its subtree");
    assert!(d.get("demo.f", None).await.memo.is_none(), "not above it");

    d.memo("demo.f", Some(b), H2, Some(json!("at b")), &[], None)
        .await;
    let half = d.get("demo.f", Some(b)).await.memo.unwrap();
    assert_eq!((&half["hash"], &half["data"]), (&json!(H2), &json!("at b")));
    let half = d.get("demo.f", Some(a)).await.memo.unwrap();
    assert_eq!(half["data"], json!("at a"));
}

#[tokio::test]
async fn a_memo_write_replaces_the_one_at_its_address() {
    let mut d = serve();
    assert!(d.put("demo:template.f", json!("{{ 'x' }}")).await.ok);
    d.memo("demo.f", None, H1, Some(json!("one")), &["/w"], Some("1h"))
        .await;
    d.memo("demo.f", None, H2, None, &[], None).await;
    let leaf = d.cache_of("demo.f", None).await;
    assert_eq!(leaf["hash"], json!(H2), "{leaf}");
    for key in ["poll", "decay", "watch"] {
        assert!(leaf.get(key).is_none(), "{key} is the old memo's: {leaf}");
    }
    assert!(d.get("demo:memo.f", None).await.data.is_none());
}

#[tokio::test]
async fn a_template_write_drops_the_hash() {
    let mut d = serve();
    assert!(d.put("demo:template.f", json!("{{ 'x' }}")).await.ok);
    d.memo("demo.f", None, H1, Some(json!("v")), &[], Some("10m"))
        .await;
    assert!(d.put("demo:template.f", json!("{{ 'y' }}")).await.ok);

    let leaf = d.cache_of("demo.f", None).await;
    assert!(leaf.get("hash").is_none(), "{leaf}");
    assert_eq!(leaf["source"], json!("memo"), "the record stays: {leaf}");
    assert_eq!(leaf["poll"], json!({"interval": "600s", "count": 1}));
    assert_eq!(
        d.get("demo:memo.f", None).await.data,
        Some(json!("v")),
        "and its value"
    );
}

#[tokio::test]
async fn a_data_write_under_the_field_drops_the_hash() {
    let mut d = serve();
    assert!(
        d.put("demo:template.f", json!("{{ comb.demo.f }}"))
            .await
            .ok
    );
    assert!(d.put("demo:template.g", json!("{{ 'g' }}")).await.ok);
    d.memo("demo.f", None, H1, Some(json!("v")), &[], None)
        .await;
    d.memo("demo.g", None, H2, Some(json!("w")), &[], None)
        .await;

    assert!(d.put("demo.f.below", json!(1)).await.ok);
    assert!(d.cache_of("demo.f", None).await.get("hash").is_none());
    assert_eq!(
        d.cache_of("demo.g", None).await["hash"],
        json!(H2),
        "a sibling's stays"
    );

    // Removing the data moves it too.
    d.memo("demo.f", None, H1, Some(json!("v")), &[], None)
        .await;
    assert!(d.put("demo.f", Json::Null).await.ok);
    assert!(d.cache_of("demo.f", None).await.get("hash").is_none());
}

/// A Source's run writes the field a cached template sits over: its hash goes.
#[tokio::test]
async fn a_source_write_under_the_field_drops_the_hash() {
    let state = TempDir::new().unwrap();
    let file = state.path().join("state");
    std::fs::write(&file, "1").unwrap();
    let config = crate::common::registry::config_over_shipped(vec![(
        PathBuf::from("/t.toml"),
        toml::from_str(&format!(
            "[daemon.providers.w.sources.s]\n\
             data = \"{{{{ '{}' | read }}}}\"\n\
             result.v = \"{{{{ data }}}}\"\n",
            file.display()
        ))
        .unwrap(),
    )]);
    let registry = ProviderRegistry::with_config(&config, &std::env::vars().collect()).unwrap();
    let mut d = serve_with(registry);
    assert!(d.put("w:template.v", json!("{{ comb.w.v }}")).await.ok);
    d.memo("w.v", None, H1, Some(json!("1")), &[], None).await;

    // The cold read runs the Source, which writes `v`.
    assert!(d.get("w.v", None).await.ok);
    assert!(d.get("w:source.v", None).await.data.is_some(), "it ran");
    let leaf = d.cache_of("w.v", None).await;
    assert!(leaf.get("hash").is_none(), "{leaf}");
}

#[tokio::test]
async fn refresh_drops_memo_hashes_and_is_ok() {
    let mut d = serve();
    assert!(
        d.put("demo:template", json!({"f": "{{ 1 }}", "g": "{{ 2 }}"}))
            .await
            .ok
    );
    let root = TempDir::new().unwrap();
    let at = canonical(&root);
    let at = at.to_str().unwrap();
    d.memo("demo.f", None, H1, Some(json!(1)), &[], None).await;
    d.memo("demo.f", Some(at), H1, Some(json!(1)), &[], None)
        .await;
    d.memo("demo.g", None, H2, Some(json!(2)), &[], None).await;

    let r = d.send(json!({"op": "refresh", "key": "demo.f"})).await;
    assert!(r.ok, "a memo is something to refresh: {:?}", r.error);
    assert!(d.cache_of("demo.f", None).await.get("hash").is_none());
    assert!(
        d.cache_of("demo.f", Some(at)).await.get("hash").is_none(),
        "at every directory"
    );
    assert_eq!(d.cache_of("demo.g", None).await["hash"], json!(H2));

    let r = d.send(json!({"op": "refresh", "key": "demo"})).await;
    assert!(r.ok, "{:?}", r.error);
    assert!(d.cache_of("demo.g", None).await.get("hash").is_none());

    // A namespace with neither a Source nor a memo has still nothing.
    assert!(d.put("plain:template.f", json!("{{ 1 }}")).await.ok);
    let r = d.send(json!({"op": "refresh", "key": "plain"})).await;
    assert_eq!(
        r.error.as_deref(),
        Some("nothing to refresh: plain has no sources")
    );
}

#[tokio::test]
async fn a_watch_frame_follows_a_hash_drop() {
    let mut d = serve();
    assert!(d.put("demo:template.f", json!("{{ 'x' }}")).await.ok);
    d.memo("demo.f", None, H1, Some(json!("v")), &[], None)
        .await;

    let stream = UnixStream::connect(&d.sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    writer
        .write_all(format!("{}\n", json!({"op": "watch", "key": "demo.f"})).as_bytes())
        .await
        .unwrap();
    let mut lines = BufReader::new(reader).lines();
    let mut frame = async || -> Json {
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .expect("a frame")
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let first = frame().await;
    assert_eq!(first["memo"]["hash"], json!(H1), "{first}");

    assert!(d.send(json!({"op": "refresh", "key": "demo.f"})).await.ok);
    let next = frame().await;
    assert!(next["memo"].get("hash").is_none(), "{next}");
    assert_eq!(next["memo"]["data"], json!("v"), "{next}");
    assert_eq!(next["template"], first["template"]);
}

#[tokio::test]
async fn a_bad_memo_request_is_refused() {
    let mut d = serve();
    let refused = async |d: &mut Served, request: Json| -> String {
        let r = d.send(request).await;
        assert!(!r.ok, "{r:?}");
        r.error.unwrap()
    };
    let memo = |key: &str, hash: &str| json!({"op": "memo", "key": key, "hash": hash});
    assert_eq!(
        refused(&mut d, memo("demo:template.f", H1)).await,
        "memo takes a provider.field key, not a facet: demo:template.f"
    );
    assert_eq!(
        refused(&mut d, memo("demo", H1)).await,
        "memo takes a provider.field key: demo names no field"
    );
    assert_eq!(
        refused(&mut d, memo("demo.f", "9c6001d328a9469")).await,
        "invalid memo hash: 9c6001d328a9469 (sixteen lowercase hex digits)"
    );
    assert_eq!(
        refused(&mut d, memo("demo.f", "9C6001D328A94690")).await,
        "invalid memo hash: 9C6001D328A94690 (sixteen lowercase hex digits)"
    );
    assert_eq!(
        refused(
            &mut d,
            json!({"op": "memo", "key": "demo.f", "hash": H1, "watch": ["rel/x"]})
        )
        .await,
        "a memo's watch paths are absolute: rel/x"
    );
    assert_eq!(
        refused(
            &mut d,
            json!({"op": "memo", "key": "demo.f", "hash": H1, "ttl": "500ms"})
        )
        .await,
        "invalid ttl: 500ms"
    );
    assert_eq!(
        refused(
            &mut d,
            json!({"op": "memo", "key": "demo.f", "hash": H1, "ttl": "0s"})
        )
        .await,
        "ttl must be at least 1s"
    );
    assert!(
        refused(&mut d, json!({"op": "memo", "key": "demo.f"}))
            .await
            .starts_with("invalid request: missing field `hash`")
    );
}
