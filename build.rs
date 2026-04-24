//! Emits BEACHCOMBER_VERSION env var for compile-time version string injection.
//!
//! Design: does NOT read git state at build time (which would invalidate the cargo
//! build cache on every commit). Reads `COMB_BUILD_SHA` / `COMB_BUILD_DIRTY` env
//! vars if set (CI injects these for release builds); otherwise emits the bare
//! cargo version. Dev builds always show `0.5.1`; CI release builds show
//! `0.5.1+sha.abc12345` or `0.5.1+sha.abc12345.dirty`.
//!
//! Binary identity for singleton enforcement is a SEPARATE concern handled at
//! runtime by hashing the daemon binary content — see `src/singleton.rs`. Build
//! ID and human version are deliberately orthogonal.

fn main() {
    // Rerun only if the injected env vars change. Git-state changes do NOT invalidate
    // the build cache — that's why we don't watch `.git/HEAD`.
    println!("cargo:rerun-if-env-changed=COMB_BUILD_SHA");
    println!("cargo:rerun-if-env-changed=COMB_BUILD_DIRTY");

    let cargo_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let sha = std::env::var("COMB_BUILD_SHA").ok().filter(|s| !s.is_empty());
    let dirty = std::env::var("COMB_BUILD_DIRTY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let version = match (sha, dirty) {
        (None, _) => cargo_version,
        (Some(s), true) => format!("{cargo_version}+sha.{s}.dirty"),
        (Some(s), false) => format!("{cargo_version}+sha.{s}"),
    };

    println!("cargo:rustc-env=BEACHCOMBER_VERSION={version}");
}
