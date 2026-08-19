//! Emits BEACHCOMBER_VERSION env var for compile-time version string injection.
//!
//! Derivation logic lives in `../build-common/version.rs`, shared (via
//! `include!`) with the root crate's `build.rs` so the two crates can never
//! disagree on the version string. See that file for the full rationale.
//!
//! Included here through `build-common/version.rs`, a symlink back to the
//! real file at the repo root (`../../build-common/version.rs` relative to
//! the symlink itself) — not a direct `../` include. `cargo package` only
//! packs files within a crate's own root, so a plain `include!("../...")`
//! would resolve locally but silently vanish from the published tarball;
//! the symlink keeps the file inside this crate's package while still being
//! one real implementation on disk.

include!("build-common/version.rs");

fn main() {
    emit_version_env();
}
