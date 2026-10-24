//! Parameterised conformance harness — every registered (provider, source)
//! pair must pass these contract assertions.
//!
//! This module defines assertion functions and the enumeration helper.
//! `tests/suite/provider_conformance.rs` wires them into a single nextest test.

use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{Source, SourceResult};

use crate::provider_conformance::conformance::fixtures::ConformanceFixture;

/// Iterate every `(provider_name, source_name, source)` triple registered by
/// the default built-in registry. Yields owned data so callers can iterate
/// without holding registry references.
pub fn enumerate_sources() -> Vec<(String, String, std::sync::Arc<dyn Source>)> {
    let registry = ProviderRegistry::with_defaults();
    let mut out = Vec::new();
    for provider_name in registry.provider_names() {
        let source_metas = match registry.provider_sources(&provider_name) {
            Some(m) => m.to_vec(),
            None => continue,
        };
        for source_meta in source_metas {
            let source_name = source_meta.name.clone();
            if let Some(arc) = registry.source(&provider_name, &source_name) {
                out.push((provider_name.clone(), source_name, arc));
            }
        }
    }
    out
}

/// Assert that the source's metadata name matches the source_name key it was
/// registered under in the registry.
pub fn assert_metadata_name(provider: &str, source_name: &str, source: &dyn Source) {
    let meta = source.metadata();
    assert_eq!(
        meta.name, source_name,
        "{provider}.{source_name}: metadata name mismatch (got '{}')",
        meta.name,
    );
}

/// Assert that roots are honoured:
/// - a Source with no root has one instance, at `~`, and produces
///   structurally-identical field key sets whichever directory it is run in.
/// - a rooted Source at least doesn't panic when given a real directory, and a
///   `root` asked about one answers without error.
///
/// A Source that fails to run both ways (a missing tool, say) has no fields to
/// compare: that is reported, not passed over.
pub fn assert_root_consistent(provider: &str, source_name: &str, source: &dyn Source) {
    let f = ConformanceFixture::empty();
    let dir = f.path.to_str().unwrap();
    if source.has_root() {
        let _ = source.execute(Some(dir));
        assert!(
            source.root(dir).is_ok(),
            "{provider}.{source_name}: root failed for a plain directory"
        );
    } else {
        assert_eq!(
            source.root(dir),
            Ok(None),
            "{provider}.{source_name}: a Source with no root answers for every directory at ~"
        );
        let with_path = source.execute(Some(dir));
        let without_path = source.execute(None);
        match (&with_path, &without_path) {
            (Ok(with_path), Ok(without_path)) => assert_eq!(
                field_key_shape(with_path),
                field_key_shape(without_path),
                "{provider}.{source_name}: a source with no root returned different field key \
                 shapes for path vs no-path",
            ),
            (Err(a), Err(b)) => eprintln!(
                "{provider}.{source_name}: not compared, it fails both ways here \
                 (with a directory: {a}; without: {b})"
            ),
            (with_path, without_path) => panic!(
                "{provider}.{source_name}: a source with no root ran for only one of a directory \
                 and none: {with_path:?} / {without_path:?}"
            ),
        }
    }
}

/// Assert that passing a path that doesn't exist on disk does not cause a
/// panic.  Sources are expected to return an empty result or failure fields,
/// not crash.
pub fn assert_missing_path_handled(provider: &str, source_name: &str, source: &dyn Source) {
    let f = ConformanceFixture::missing();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        source
            .execute(Some(f.path.to_str().unwrap()))
            .unwrap_or_default()
    }));
    assert!(
        result.is_ok(),
        "{provider}.{source_name}: panicked on missing path",
    );
}

/// Assert that the fields returned by `execute()` round-trip through serde_json.
///
/// `SourceResult` itself is not `Serialize`/`Deserialize`, but its inner
/// `HashMap<String, Value>` (where `Value` fully derives both) must survive a
/// JSON round-trip.
pub fn assert_serialization_roundtrip(provider: &str, source_name: &str, source: &dyn Source) {
    let f = ConformanceFixture::empty();
    let result = source
        .execute(Some(f.path.to_str().unwrap()))
        .unwrap_or_default();
    let json = serde_json::to_string(&result.fields)
        .unwrap_or_else(|e| panic!("{provider}.{source_name}: serialize fields: {e}"));
    let back: std::collections::HashMap<String, beachcomber::provider::Value> =
        serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("{provider}.{source_name}: deserialize fields: {e}"));
    // Re-check key set survives the round-trip.
    let original_keys: std::collections::BTreeSet<String> = result.fields.keys().cloned().collect();
    let roundtrip_keys: std::collections::BTreeSet<String> = back.keys().cloned().collect();
    assert_eq!(
        original_keys, roundtrip_keys,
        "{provider}.{source_name}: field keys changed after serde round-trip",
    );
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Return a sorted vec of field key names from a `SourceResult`.
/// Used as a "shape signature" for root-consistency comparisons.
fn field_key_shape(r: &SourceResult) -> Vec<String> {
    let mut keys: Vec<String> = r.fields.keys().cloned().collect();
    keys.sort();
    keys
}
