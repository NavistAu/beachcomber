//! Emits BEACHCOMBER_VERSION env var for compile-time version string injection.
//!
//! Preference order for the sha/dirty components:
//!   1. `COMB_BUILD_SHA` / `COMB_BUILD_DIRTY` env vars (CI release builds inject
//!      these explicitly).
//!   2. Shell out to `git rev-parse` / `git diff-index` for local builds.
//!   3. Bare `CARGO_PKG_VERSION` if neither source is available (e.g. tarball
//!      builds without a `.git` dir).
//!
//! Rebuild tracking: we watch `.git/HEAD` and `.git/index` so new commits,
//! branch switches, and staging changes invalidate the cached version. This
//! means commits trigger a single incremental rustc pass on consumers of the
//! env var (currently just `main.rs`). Pure working-tree edits (unstaged) do
//! NOT retrigger build.rs — the dirty flag captured at last invocation may lag
//! until the next stage or source change is picked up elsewhere. Acceptable
//! trade-off; the sha is what uniquely identifies the commit.
//!
//! Binary identity for singleton enforcement is a SEPARATE concern handled at
//! runtime by hashing the daemon binary content — see `src/singleton.rs`. Build
//! ID and human version are deliberately orthogonal.

fn main() {
    println!("cargo:rerun-if-env-changed=COMB_BUILD_SHA");
    println!("cargo:rerun-if-env-changed=COMB_BUILD_DIRTY");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");

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
