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
