// Shared `BEACHCOMBER_VERSION` derivation, `include!`-d by every crate's
// `build.rs` (root `beachcomber`, `beachcomber-client`, and eventually
// `beachcomber-ffi`) so they can never disagree on the version string.
//
// Preference order for the sha/dirty components:
//   1. `COMB_BUILD_SHA` / `COMB_BUILD_DIRTY` env vars (CI release builds inject
//      these explicitly).
//   2. Shell out to `git rev-parse` / `git diff-index` for local builds.
//   3. Bare `CARGO_PKG_VERSION` if neither source is available (e.g. tarball
//      builds without a `.git` dir).
//
// Rebuild tracking: we watch the real `HEAD` and `index` files so new commits,
// branch switches, and staging changes invalidate the cached version. This
// means commits trigger a single incremental rustc pass on consumers of the
// env var (currently just `main.rs`). Pure working-tree edits (unstaged) do
// NOT retrigger build.rs — the dirty flag captured at last invocation may lag
// until the next stage or source change is picked up elsewhere. Acceptable
// trade-off; the sha is what uniquely identifies the commit.
//
// In a plain checkout `.git` is a directory and those files live directly
// under it. In a linked worktree (`git worktree add`) `.git` is a *file*
// containing `gitdir: <path>`, and the real per-worktree `HEAD`/`index` live
// at that path instead — watching the literal `.git/HEAD` there watches a
// path that can never exist, so Cargo reruns the build script (and the full
// crate rebuild that implies) on every invocation. `git_dir()` resolves the
// real directory in both cases; paths that still don't resolve are simply
// not watched rather than watched-and-missing. Callers may live below the
// repo root (e.g. `beachcomber-client/`), so `git_dir()` walks up from
// `CARGO_MANIFEST_DIR` until it finds a `.git` entry.
//
// Binary identity for singleton enforcement is a SEPARATE concern handled at
// runtime by hashing the daemon binary content — see `src/singleton.rs`. Build
// ID and human version are deliberately orthogonal.

/// Emits `cargo:rerun-if-changed` / `cargo:rustc-env` for `BEACHCOMBER_VERSION`.
/// Called from each crate's `build.rs::main()`.
fn emit_version_env() {
    println!("cargo:rerun-if-env-changed=COMB_BUILD_SHA");
    println!("cargo:rerun-if-env-changed=COMB_BUILD_DIRTY");
    if let Some(dir) = git_dir() {
        let head = dir.join("HEAD");
        if head.exists() {
            println!("cargo:rerun-if-changed={}", head.display());
        }
        let index = dir.join("index");
        if index.exists() {
            println!("cargo:rerun-if-changed={}", index.display());
        }
    }

    let cargo_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());

    let sha = std::env::var("COMB_BUILD_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(git_head_sha);
    let dirty = match std::env::var("COMB_BUILD_DIRTY") {
        Ok(v) => v == "1" || v.eq_ignore_ascii_case("true"),
        Err(_) => git_is_dirty(),
    };

    let version = match (sha, dirty) {
        (None, _) => cargo_version,
        (Some(s), true) => format!("{cargo_version}+sha.{s}.dirty"),
        (Some(s), false) => format!("{cargo_version}+sha.{s}"),
    };

    println!("cargo:rustc-env=BEACHCOMBER_VERSION={version}");
}

/// Resolves the actual git directory containing `HEAD` and `index`.
///
/// `.git` is a directory in a normal checkout, but a file (`gitdir: <path>`)
/// in a linked worktree, pointing at `.git/worktrees/<name>` in the main
/// repo's git dir. Walks up from `CARGO_MANIFEST_DIR` (the crate's own root,
/// which may be a subdirectory of the repo root) to find it.
///
/// The walk is bounded: it only continues into a parent directory that itself
/// contains a `Cargo.toml`, i.e. still looks like part of the same cargo
/// project tree. This matters for crates.io installs, which build from
/// `~/.cargo/registry/src/...` — an unpacked tarball with no `Cargo.toml`
/// above it and no `.git` anywhere in reach. Without the bound, an
/// unqualified walk-to-filesystem-root could climb past the registry cache
/// into an unrelated repo further up (e.g. a git-managed `$HOME`) and stamp a
/// foreign sha. Returns `None` if no `.git` is found within that bound, or it
/// is unparseable.
fn git_dir() -> Option<std::path::PathBuf> {
    let start = std::env::var("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut dir = start.as_path();
    loop {
        if !dir.join("Cargo.toml").exists() {
            return None;
        }
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git);
        }
        if dot_git.is_file() {
            let contents = std::fs::read_to_string(&dot_git).ok()?;
            let raw = contents.trim().strip_prefix("gitdir:")?.trim();
            let path = std::path::Path::new(raw);
            return Some(if path.is_absolute() {
                path.to_path_buf()
            } else {
                dir.join(path)
            });
        }
        dir = dir.parent()?;
    }
}

fn git_head_sha() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

fn git_is_dirty() -> bool {
    // `diff-index --quiet` exits non-zero when the working tree differs from HEAD.
    // `--` disambiguates in case someone has a file named `HEAD`.
    match std::process::Command::new("git")
        .args(["diff-index", "--quiet", "HEAD", "--"])
        .output()
    {
        Ok(o) => !o.status.success(),
        Err(_) => false,
    }
}
