//! Emits BEACHCOMBER_VERSION env var for compile-time version string injection.
//!
//! Derivation logic lives in `build-common/version.rs`, shared (via `include!`)
//! with `libbeachcomber/build.rs` so the two crates can never disagree on
//! the version string. See that file for the full rationale.

include!("build-common/version.rs");

fn main() {
    emit_version_env();
}
