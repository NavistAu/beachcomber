/// Seam tests for the `kubecontext` provider.
///
/// Uses `kubecontext_source_with_path` to inject a tempdir-rooted kubeconfig
/// YAML file, making every test independent of `~/.kube/config` and
/// `$KUBECONFIG`.
///
/// Coverage:
/// - current_context_returns_value — kubeconfig with `current-context`; value
///   is returned in the `context` field.
/// - missing_kubeconfig_returns_empty — nonexistent path; source returns empty,
///   no panic.
/// - malformed_kubeconfig_returns_failure — garbage file; source returns empty,
///   no panic.
/// - multiple_contexts_picks_current — kubeconfig with several contexts; the
///   `current-context` entry is honored, not the first.
/// - empty_kubeconfig_returns_empty — empty file; source returns empty result.
use beachcomber::provider::Value;
use beachcomber::provider::kubecontext::kubecontext_source_with_path;
use std::io::Write;
use tempfile::NamedTempFile;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn write_kubeconfig(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().expect("NamedTempFile::new");
    f.write_all(content.as_bytes()).expect("write kubeconfig");
    f
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn current_context_returns_value() {
    let yaml = "\
apiVersion: v1
kind: Config
current-context: my-cluster
contexts:
- context:
    cluster: my-cluster
    namespace: production
  name: my-cluster
";
    let f = write_kubeconfig(yaml);
    let source = kubecontext_source_with_path(f.path().to_path_buf());
    let result = source.execute(None);

    assert_eq!(
        result.fields.get("context").unwrap().as_text(),
        "my-cluster"
    );
    assert_eq!(
        result.fields.get("namespace").unwrap().as_text(),
        "production"
    );
}

#[test]
fn missing_kubeconfig_returns_empty() {
    // Point at a path that does not exist.
    let path = std::path::PathBuf::from("/tmp/beachcomber_test_nonexistent_kubeconfig_abc123");
    let source = kubecontext_source_with_path(path);
    let result = source.execute(None);

    assert!(
        result.fields.is_empty(),
        "missing file should produce empty result, got: {:?}",
        result.fields
    );
}

#[test]
fn malformed_kubeconfig_returns_failure() {
    // Garbage content — not valid YAML, no `current-context:` key.
    let f = write_kubeconfig("not: valid: kubeconfig: garbage !!!\n\x00\x01\x02\n");
    let source = kubecontext_source_with_path(f.path().to_path_buf());

    // Must not panic; result should be empty (no current-context found).
    let result = source.execute(None);
    assert!(
        result.fields.is_empty(),
        "malformed kubeconfig should produce empty result, got: {:?}",
        result.fields
    );
}

#[test]
fn multiple_contexts_picks_current() {
    // Three contexts; `current-context` names the second one.
    let yaml = "\
apiVersion: v1
kind: Config
current-context: staging
contexts:
- context:
    cluster: prod-cluster
    namespace: default
  name: production
- context:
    cluster: staging-cluster
    namespace: staging-ns
  name: staging
- context:
    cluster: dev-cluster
    namespace: dev-ns
  name: development
";
    let f = write_kubeconfig(yaml);
    let source = kubecontext_source_with_path(f.path().to_path_buf());
    let result = source.execute(None);

    assert_eq!(result.fields.get("context").unwrap().as_text(), "staging");
    assert_eq!(
        result.fields.get("namespace").unwrap().as_text(),
        "staging-ns"
    );
}

#[test]
fn empty_kubeconfig_returns_empty() {
    let f = write_kubeconfig("");
    let source = kubecontext_source_with_path(f.path().to_path_buf());
    let result = source.execute(None);

    assert!(
        result.fields.is_empty(),
        "empty file should produce empty result, got: {:?}",
        result.fields
    );
}

#[test]
fn context_without_namespace_defaults_to_default() {
    // A context entry that omits the namespace field → provider returns "default".
    let yaml = "\
apiVersion: v1
kind: Config
current-context: no-ns-context
contexts:
- context:
    cluster: some-cluster
  name: no-ns-context
";
    let f = write_kubeconfig(yaml);
    let source = kubecontext_source_with_path(f.path().to_path_buf());
    let result = source.execute(None);

    assert_eq!(
        result.fields.get("context").unwrap().as_text(),
        "no-ns-context"
    );
    assert_eq!(
        result.fields.get("namespace").unwrap().as_text(),
        "default",
        "missing namespace in context block should fall back to \"default\""
    );
}

#[test]
fn current_context_present_but_empty_returns_empty() {
    // `current-context:` with no value after the colon → treated as empty → no result.
    let yaml = "\
apiVersion: v1
kind: Config
current-context:
contexts: []
";
    let f = write_kubeconfig(yaml);
    let source = kubecontext_source_with_path(f.path().to_path_buf());
    let result = source.execute(None);

    assert!(
        result.fields.is_empty(),
        "blank current-context value should produce empty result, got: {:?}",
        result.fields
    );
}

#[test]
fn context_field_is_string_value() {
    let yaml = "\
apiVersion: v1
kind: Config
current-context: dev
contexts:
- context:
    cluster: dev-cluster
    namespace: dev
  name: dev
";
    let f = write_kubeconfig(yaml);
    let source = kubecontext_source_with_path(f.path().to_path_buf());
    let result = source.execute(None);

    // Confirm the Value variant is String, not some other type.
    assert!(
        matches!(result.fields.get("context"), Some(Value::String(_))),
        "context field must be Value::String"
    );
    assert!(
        matches!(result.fields.get("namespace"), Some(Value::String(_))),
        "namespace field must be Value::String"
    );
}

// ── Multi-file merge tests ─────────────────────────────────────────────────────

/// Expose a test-only constructor that accepts multiple kubeconfig paths.
/// This tests the merge logic without touching $KUBECONFIG.
use beachcomber::provider::kubecontext::kubecontext_source_with_paths;

#[test]
fn multi_file_kubeconfig_active_context_in_second_file() {
    // File 1: defines context "prod" but current-context is NOT here.
    let f1 = write_kubeconfig(
        "\
apiVersion: v1
kind: Config
contexts:
- context:
    cluster: prod-cluster
    namespace: production
  name: prod
",
    );
    // File 2: defines context "dev" and declares it as current-context.
    let f2 = write_kubeconfig(
        "\
apiVersion: v1
kind: Config
current-context: dev
contexts:
- context:
    cluster: dev-cluster
    namespace: dev-ns
  name: dev
",
    );

    let source =
        kubecontext_source_with_paths(vec![f1.path().to_path_buf(), f2.path().to_path_buf()]);
    let result = source.execute(None);

    assert_eq!(
        result.fields.get("context").unwrap().as_text(),
        "dev",
        "current-context in second file must be honored"
    );
    assert_eq!(result.fields.get("namespace").unwrap().as_text(), "dev-ns");
}

#[test]
fn multi_file_kubeconfig_context_namespace_from_different_file() {
    // current-context set in file 2, but the context's namespace is defined in file 1.
    let f1 = write_kubeconfig(
        "\
apiVersion: v1
kind: Config
contexts:
- context:
    cluster: shared-cluster
    namespace: shared-ns
  name: shared
",
    );
    let f2 = write_kubeconfig(
        "\
apiVersion: v1
kind: Config
current-context: shared
contexts: []
",
    );

    let source =
        kubecontext_source_with_paths(vec![f1.path().to_path_buf(), f2.path().to_path_buf()]);
    let result = source.execute(None);

    assert_eq!(result.fields.get("context").unwrap().as_text(), "shared");
    assert_eq!(
        result.fields.get("namespace").unwrap().as_text(),
        "shared-ns",
        "namespace must be found even when context definition is in a different file"
    );
}

#[test]
fn name_match_not_a_substring() {
    // Context named "prod" must not be confused with "production".
    // The substring bug: `block.contains("name: prod")` fires on a block
    // that has `name: production` because "name: production" contains the
    // substring "name: prod". Verified against kubecontext.rs lines 170-171.
    let f = write_kubeconfig(
        "\
apiVersion: v1
kind: Config
current-context: prod
contexts:
- context:
    cluster: production-cluster
    namespace: wrong-ns
  name: production
- context:
    cluster: prod-cluster
    namespace: correct-ns
  name: prod
",
    );
    let source = kubecontext_source_with_path(f.path().to_path_buf());
    let result = source.execute(None);

    assert_eq!(result.fields.get("context").unwrap().as_text(), "prod");
    assert_eq!(
        result.fields.get("namespace").unwrap().as_text(),
        "correct-ns",
        "exact name match required — 'prod' must not match context named 'production'"
    );
}

#[test]
fn nonexistent_files_in_list_are_skipped() {
    let f = write_kubeconfig(
        "\
apiVersion: v1
kind: Config
current-context: real
contexts:
- context:
    namespace: real-ns
  name: real
",
    );
    let missing = std::path::PathBuf::from("/tmp/beachcomber_test_nonexistent_s6_kube");
    let source = kubecontext_source_with_paths(vec![missing, f.path().to_path_buf()]);
    let result = source.execute(None);

    assert_eq!(result.fields.get("context").unwrap().as_text(), "real");
}
