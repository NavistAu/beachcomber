//! Parameterised conformance harness — every registered (provider, source)
//! pair must pass these contract assertions.
//!
//! This module defines assertion functions and the enumeration helper.
//! `tests/provider_conformance.rs` wires them into a single nextest test.

use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{Source, SourceResult, SourceScope};

use crate::conformance::fixtures::ConformanceFixture;

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

/// Assert that scope is honoured:
/// - `Global` sources produce structurally-identical field key sets regardless
///   of whether a path is supplied.
/// - `PathScoped` sources at least don't panic when given a real path.
pub fn assert_scope_consistent(provider: &str, source_name: &str, source: &dyn Source) {
    let meta = source.metadata();
    match meta.scope {
        SourceScope::Global => {
            // Global sources must return the same field-key shape whether a
            // path is supplied or not.
            let f = ConformanceFixture::empty();
            let with_path = source.execute(Some(f.path.to_str().unwrap()));
            let without_path = source.execute(None);
            assert_eq!(
                field_key_shape(&with_path),
                field_key_shape(&without_path),
                "{provider}.{source_name}: global source returned different field key shapes \
                 for path vs no-path",
            );
        }
        SourceScope::PathScoped => {
            // PathScoped sources just need to not panic when given a real path.
            let f = ConformanceFixture::empty();
            let _ = source.execute(Some(f.path.to_str().unwrap()));
        }
    }
}

/// Assert that passing a path that doesn't exist on disk does not cause a
/// panic.  Sources are expected to return an empty result or failure fields,
/// not crash.
pub fn assert_missing_path_handled(provider: &str, source_name: &str, source: &dyn Source) {
    let f = ConformanceFixture::missing();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        source.execute(Some(f.path.to_str().unwrap()))
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
    let result = source.execute(Some(f.path.to_str().unwrap()));
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
/// Used as a "shape signature" for scope-consistency comparisons.
fn field_key_shape(r: &SourceResult) -> Vec<String> {
    let mut keys: Vec<String> = r.fields.keys().cloned().collect();
    keys.sort();
    keys
}
