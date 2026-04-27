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
            ("scope_consistent", &|| {
                assert_scope_consistent(&provider, &source_name, source.as_ref())
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
