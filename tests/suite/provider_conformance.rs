// tests/conformance/ stays put (not nested under tests/suite/) because the
// SDK conformance runners (sdks/*/conformance_runner.*) hardcode its path
// relative to the repo root and read its fixtures directly.
#[path = "../conformance/mod.rs"]
mod conformance;

use conformance::provider_harness::*;

#[test]
fn all_sources_pass_conformance() {
    let mut failures: Vec<String> = Vec::new();

    for (provider, source_name, source) in enumerate_sources() {
        let runs: &[(&str, &dyn Fn())] = &[
            ("metadata_name", &|| {
                assert_metadata_name(&provider, &source_name, source.as_ref())
            }),
            ("root_consistent", &|| {
                assert_root_consistent(&provider, &source_name, source.as_ref())
            }),
            ("missing_path_handled", &|| {
                assert_missing_path_handled(&provider, &source_name, source.as_ref())
            }),
            ("serialization_roundtrip", &|| {
                assert_serialization_roundtrip(&provider, &source_name, source.as_ref())
            }),
        ];

        for (name, run) in runs {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
            if outcome.is_err() {
                failures.push(format!("{provider}.{source_name}: {name}"));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "conformance failures (count={}):\n{}",
        failures.len(),
        failures.join("\n"),
    );
}

/// A Source switched off is not run here, its siblings are.
#[test]
fn the_harness_skips_a_disabled_source() {
    let layer: toml::Value = toml::from_str(
        "[daemon.providers.cfx.sources.on]\ndata = \"{{ 1 }}\"\nresult.a = \"{{ data }}\"\n\
         [daemon.providers.cfx.sources.off]\nenabled = false\ndata = \"{{ 2 }}\"\n\
         result.b = \"{{ data }}\"\n",
    )
    .unwrap();
    let config = crate::common::registry::config_over_shipped(vec![("/t/cfx.toml".into(), layer)]);
    let registry = beachcomber::provider::registry::ProviderRegistry::with_config(
        &config,
        &std::env::vars().collect(),
    )
    .unwrap();
    let cfx: Vec<String> = sources_of(&registry)
        .into_iter()
        .filter(|(p, _, _)| p == "cfx")
        .map(|(_, s, _)| s)
        .collect();
    assert_eq!(cfx, ["on"]);
}
