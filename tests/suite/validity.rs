//! A warm read checks its record's stat inputs (phase 4 §2.2–§2.4).
//!
//! The server here runs with no scheduler, so nothing watches and nothing
//! polls: a read that answers the new value found the change itself.

use crate::common::git::GitRepoFixture;
use beachcomber::cache::Cache;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use beachcomber::watcher_registry::WatcherRegistry;
use serde_json::{Value as Json, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// An in-process server of the shipped defaults,
/// with `toml` over them, and no scheduler.
struct Served {
    sock: PathBuf,
    cache: Arc<Cache>,
    _dir: tempfile::TempDir,
}

fn serve(toml: &str) -> Served {
    let layer: toml::Value = toml::from_str(toml).unwrap();
    let config =
        crate::common::registry::config_over_shipped(vec![(PathBuf::from("test.toml"), layer)]);
    let load = ProviderRegistry::load(&config, &std::env::vars().collect());
    assert!(load.errors.is_empty(), "{:?}", load.errors);
    let watchers = Arc::new(WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = load.registry.share(&cache);
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("s");
    Server::new(sock.clone(), cache.clone(), registry, None, watchers)
        .spawn()
        .unwrap();
    Served {
        sock,
        cache,
        _dir: dir,
    }
}

impl Served {
    async fn ask(&self, request: Json) -> Json {
        let mut stream = UnixStream::connect(&self.sock).await.unwrap();
        let mut line = request.to_string();
        line.push('\n');
        stream.write_all(line.as_bytes()).await.unwrap();
        let mut answer = String::new();
        BufReader::new(stream).read_line(&mut answer).await.unwrap();
        serde_json::from_str(&answer).expect("a JSON answer")
    }

    /// `key` at `dir` (the global entry for `None`), waiting for any run the
    /// read makes: what is under test is whether it makes one, not how fast.
    async fn get(&self, key: &str, dir: Option<&Path>) -> Json {
        let mut request = json!({"op": "get", "key": key, "wait": "forever"});
        if let Some(dir) = dir {
            request["path"] = json!(dir);
        }
        self.ask(request).await
    }

    /// When the record `source` of `provider` at the global entry was last
    /// written: unchanged across reads that ran nothing.
    fn written(&self, provider: &str, source: &str) -> tokio::time::Instant {
        self.written_at(provider, None, source)
    }

    /// [`Self::written`] for the instance at `root` (`None`, the global
    /// entry).
    fn written_at(
        &self,
        provider: &str,
        root: Option<&Path>,
        source: &str,
    ) -> tokio::time::Instant {
        let root = root.map(|r| r.to_string_lossy().into_owned());
        self.cache
            .get_source(provider, root.as_deref(), source)
            .expect("a record")
            .last_refreshed
    }
}

/// Give `path` an mtime well in the past, so a stamp of it is not racy.
fn age(path: &Path) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(10))
        .unwrap();
}

// ── a checkout is seen by the next read ─────────────────────────────────────

/// Point `gitdir`'s HEAD at `branch` as git does: a lockfile renamed over it.
fn write_head(gitdir: &Path, branch: &str) {
    let lock = gitdir.join("HEAD.lock");
    std::fs::write(&lock, format!("ref: refs/heads/{branch}\n")).unwrap();
    std::fs::rename(&lock, gitdir.join("HEAD")).unwrap();
}

/// At `dir`, on `first`, whose HEAD is `gitdir/HEAD`: three real checkouts,
/// then ten HEAD writes of one size, as fast as they can be made, each read
/// straight after.
async fn every_checkout_is_seen(repo: &GitRepoFixture, dir: &Path, gitdir: &Path, first: &str) {
    let s = serve("");
    repo.git_in(dir, &["branch", "one"]);
    repo.git_in(dir, &["branch", "two"]);
    let branch = |answer: Json| answer["data"].clone();
    assert_eq!(branch(s.get("git.branch", Some(dir)).await), json!(first));
    for name in ["one", "two", "one"] {
        repo.git_in(dir, &["checkout", "-q", name]);
        assert_eq!(
            branch(s.get("git.branch", Some(dir)).await),
            json!(name),
            "git checkout {name}"
        );
    }
    for i in 0..10 {
        let name = format!("b{i}");
        write_head(gitdir, &name);
        assert_eq!(
            branch(s.get("git.branch", Some(dir)).await),
            json!(name),
            "HEAD write {i}"
        );
    }
}

#[tokio::test]
async fn a_checkout_is_seen_by_the_next_read() {
    let repo = GitRepoFixture::new();
    let dir = repo.path().canonicalize().unwrap();
    every_checkout_is_seen(&repo, &dir, &dir.join(".git"), "main").await;
}

/// A linked worktree's HEAD is in its own gitdir, which its `.git` file
/// names.
#[tokio::test]
async fn a_checkout_in_a_linked_worktree_is_seen_by_the_next_read() {
    let repo = GitRepoFixture::new();
    let wt = repo.add_worktree("wt", "start").canonicalize().unwrap();
    let gitdir = repo
        .path()
        .canonicalize()
        .unwrap()
        .join(".git/worktrees/wt");
    every_checkout_is_seen(&repo, &wt, &gitdir, "start").await;
}

/// `git.branch` is answered from its record while HEAD is as it was: its
/// age grows across reads, where a Source run on every read is always new.
#[tokio::test]
async fn a_valid_branch_is_answered_from_its_record() {
    let repo = GitRepoFixture::new();
    let dir = repo.path().canonicalize().unwrap();
    age(&dir.join(".git/HEAD"));
    let s = serve("");
    let first = s.get("git.branch", Some(&dir)).await;
    assert_eq!(first["data"], json!("main"));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let again = s.get("git.branch", Some(&dir)).await;
        assert_eq!(again["data"], json!("main"));
        if again["age_ms"].as_u64().is_some_and(|age| age >= 2) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the record was written again by a read: {again}"
        );
        // age_ms counts in whole milliseconds: this waits for two of them.
        // sleep-justified: a millisecond at a time, a few times at most.
        std::thread::sleep(Duration::from_millis(1));
    }
}

// ── a config Source's declared files ────────────────────────────────────────

/// A config Source `w.s` whose value is `state`'s content, with `decorators`
/// on its `return` line.
fn config_source(state: &Path, decorators: &str) -> String {
    format!(
        "[daemon.providers.w.sources.s]\n\
         data = \"{{{{ '{}' | read }}}}\"\n\
         result.v = \"{{{{ data }}}}\"\n\
         return = \"{{{{ result | {decorators} }}}}\"\n",
        state.display()
    )
}

/// A rewrite of a watched file runs the Source again on the next read; while
/// nothing changes, reads run nothing.
#[tokio::test]
async fn a_watched_file_rewritten_runs_its_source_again() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    std::fs::write(&state, "1").unwrap();
    age(&state);
    let s = serve(&config_source(
        &state,
        &format!("watch('{}')", state.display()),
    ));
    assert_eq!(s.get("w.v", None).await["data"], json!("1"));
    let written = s.written("w", "s");
    for _ in 0..50 {
        assert_eq!(s.get("w.v", None).await["data"], json!("1"));
    }
    assert_eq!(s.written("w", "s"), written, "50 reads ran it 0 times");
    std::fs::write(&state, "2").unwrap();
    assert_eq!(s.get("w.v", None).await["data"], json!("2"));
}

/// An input that did not exist when the Source ran is one whose creation is
/// a change.
#[tokio::test]
async fn an_absent_input_created_runs_its_source_again() {
    let dir = tempfile::tempdir().unwrap();
    let (state, flag) = (dir.path().join("state.json"), dir.path().join("flag"));
    std::fs::write(&state, "1").unwrap();
    age(&state);
    let s = serve(&config_source(
        &state,
        &format!("watch('{}', '{}')", state.display(), flag.display()),
    ));
    assert_eq!(s.get("w.v", None).await["data"], json!("1"));
    let written = s.written("w", "s");
    s.get("w.v", None).await;
    assert_eq!(s.written("w", "s"), written);
    std::fs::write(&flag, "").unwrap();
    s.get("w.v", None).await;
    assert_ne!(s.written("w", "s"), written, "the created input ran it");
}

/// A Source that only polls and whose lines read no file has no stat inputs:
/// a read checks only its lifetime, and the record has no hash. What a child
/// process reads is not the lines' reading.
#[tokio::test]
async fn a_source_that_only_polls_is_checked_for_its_lifetime_only() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    std::fs::write(&state, "1").unwrap();
    let s = serve(&format!(
        "[daemon.providers.w.sources.s]\n\
         data = \"{{{{ exec(['cat', '{}']) }}}}\"\n\
         result.v = \"{{{{ data }}}}\"\n\
         return = \"{{{{ result | poll('60s') }}}}\"\n",
        state.display()
    ));
    assert_eq!(s.get("w.v", None).await["data"], json!("1"));
    std::fs::write(&state, "2").unwrap();
    assert_eq!(
        s.get("w.v", None).await["data"],
        json!("1"),
        "within its lifetime, as stored"
    );
    let cache = s.get("w:cache.v", None).await;
    assert!(cache["data"].get("hash").is_none(), "{cache}");
}

/// A same-size write in place within the racy window leaves the stamp as it
/// was (the mtime restored stands in for a filesystem too coarse to move
/// it): the record is run again once the window has closed.
#[tokio::test]
async fn a_racy_same_size_write_is_caught_after_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    std::fs::write(&state, "0").unwrap();
    let s = serve(&config_source(
        &state,
        &format!("watch('{}')", state.display()),
    ));
    // Each attempt gives the file an mtime 50 ms ahead of the clock, so the
    // run's stamp is racy however long the read takes to make it, up to that;
    // an attempt whose read took longer proves nothing, and the next is made.
    for attempt in 0..10 {
        let (before, after) = (format!("{attempt}a"), format!("{attempt}b"));
        let mtime = SystemTime::now() + Duration::from_millis(50);
        let write = |body: &str| {
            std::fs::write(&state, body).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&state)
                .unwrap()
                .set_modified(mtime)
                .unwrap();
        };
        write(&before);
        assert_eq!(s.get("w.v", None).await["data"], json!(before));
        write(&after);
        if SystemTime::now() >= mtime {
            continue;
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while s.get("w.v", None).await["data"] != json!(after) {
            assert!(
                std::time::Instant::now() < deadline,
                "the racy record was never run again"
            );
            // The window closes 60 ms after the write, on the wall clock.
            // sleep-justified: it is waited out a millisecond at a time.
            std::thread::sleep(Duration::from_millis(1));
        }
        return;
    }
    panic!("no first read took under 50 ms in ten attempts");
}

// ── what a config Source's lines read ──────────────────────────────────────

/// A config Source `r.s` that declares no file (it polls), rooted at the
/// directory holding `.rmark`, whose value is the nearest `state.json` above
/// its root: the files it checks are the ones its lines read.
const READS_NEAREST: &str = "[daemon.providers.r.sources.s]\n\
     root = \"{{ lup('.rmark') | dirname }}\"\n\
     data = \"{{ lup('state.json') | read }}\"\n\
     result.v = \"{{ data }}\"\n\
     return = \"{{ result | poll('10m') }}\"\n";

/// With no `watch()`, a rewrite of the file `data` read is seen by the next
/// read, every time; so is a `state.json` made nearer than the one it found;
/// and while nothing changes, reads run nothing.
#[tokio::test]
async fn a_file_the_lines_read_is_checked_by_the_next_read() {
    let dir = tempfile::tempdir().unwrap();
    let top = dir.path().canonicalize().unwrap();
    let project = top.join("a/project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join(".rmark"), "").unwrap();
    let far = top.join("state.json");
    std::fs::write(&far, "far").unwrap();
    age(&far);
    let s = serve(READS_NEAREST);
    let v = |answer: Json| answer["data"].clone();
    assert_eq!(v(s.get("r.v", Some(&project)).await), json!("far"));
    let written = s.written_at("r", Some(&project), "s");
    for _ in 0..50 {
        assert_eq!(v(s.get("r.v", Some(&project)).await), json!("far"));
    }
    assert_eq!(
        s.written_at("r", Some(&project), "s"),
        written,
        "50 reads ran it 0 times"
    );

    let near = top.join("a/state.json");
    std::fs::write(&near, "near").unwrap();
    assert_eq!(
        v(s.get("r.v", Some(&project)).await),
        json!("near"),
        "a marker made nearer"
    );
    for i in 0..100 {
        std::fs::write(&near, format!("v{i}")).unwrap();
        assert_eq!(
            v(s.get("r.v", Some(&project)).await),
            json!(format!("v{i}")),
            "rewrite {i}"
        );
    }
}

/// A run that reads more files than a record is checked for keeps the first
/// 64: a change to one of them is seen by the next read, one to a later file
/// waits for the poll, and the Source answers all the same.
#[tokio::test]
async fn a_source_that_reads_100_files_is_checked_for_its_first_64() {
    let dir = tempfile::tempdir().unwrap();
    let file = |i: usize| dir.path().join(format!("f{i}"));
    for i in 0..100 {
        std::fs::write(file(i), "1").unwrap();
        age(&file(i));
    }
    let s = serve(&format!(
        "[daemon.providers.many.sources.s]\n\
         data = \"{{% for i in range(100) %}}{{{{ ('{}/f' ~ i) | read }}}}{{% endfor %}}\"\n\
         result.n = \"{{{{ data | length }}}}\"\n\
         return = \"{{{{ result | poll('10m') }}}}\"\n",
        dir.path().display()
    ));
    assert_eq!(s.get("many.n", None).await["data"], json!(100));
    std::fs::write(file(80), "22").unwrap();
    assert_eq!(
        s.get("many.n", None).await["data"],
        json!(100),
        "the 81st file read is not checked"
    );
    std::fs::write(file(10), "22").unwrap();
    assert_eq!(
        s.get("many.n", None).await["data"],
        json!(102),
        "the 11th is"
    );
}

/// The `hash` of a field's `:cache` answer is its record's validity hash, 16
/// lowercase hex digits, written by the daemon alone.
#[tokio::test]
async fn the_hash_is_shown_in_cache_and_refuses_a_put() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    std::fs::write(&state, "1").unwrap();
    age(&state);
    let s = serve(&config_source(
        &state,
        &format!("watch('{}')", state.display()),
    ));
    s.get("w.v", None).await;
    let cache = s.get("w:cache.v", None).await;
    let hash = cache["data"]["hash"]
        .as_str()
        .unwrap_or_else(|| panic!("{cache}"))
        .to_string();
    assert_eq!(hash.len(), 16, "{hash}");
    assert!(
        hash.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{hash}"
    );
    let put = s
        .ask(json!({"op": "put", "key": "w:cache.v.hash", "data": "0000000000000000"}))
        .await;
    assert_eq!(put["ok"], json!(false), "{put}");
    std::fs::write(&state, "22").unwrap();
    s.get("w.v", None).await;
    let again = s.get("w:cache.v", None).await;
    assert_ne!(again["data"]["hash"], json!(hash), "a new run, a new hash");
    assert_eq!(s.get("w.v", None).await["data"], json!("22"));
}
