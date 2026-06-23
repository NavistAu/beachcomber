/// Tests for the `kubecontext` provider (PathScoped + Watch).
///
/// The source receives the kubeconfig path (single or ':'-joined list) via the
/// `path` argument to `execute`, matching the PathScoped contract. Tests use
/// tempdir-rooted kubeconfig YAML files so each test is independent of
/// `~/.kube/config` and `$KUBECONFIG`.
///
/// Coverage:
/// - source metadata declares PathScoped scope and Watch invalidation
/// - execute(Some(single_file)) returns context + namespace from that file
/// - execute(Some("fileA:fileB")) merges; later file's current-context wins
/// - watched_files(Some("a:b")) returns [PathBuf("a"), PathBuf("b")]
/// - execute(None) returns empty
/// - missing file returns empty (no panic)
/// - malformed file returns empty (no panic)
/// - multiple contexts in one file: current-context is honored
/// - context without namespace defaults to "default"
/// - blank current-context value returns empty
/// - context field is Value::String
/// - name match is exact (not substring)
/// - nonexistent files in a ':'-joined list are skipped
use beachcomber::provider::kubecontext::KubecontextProvider;
use beachcomber::provider::{InvalidationStrategy, Provider, SourceScope, Value};
use std::io::Write;
use tempfile::NamedTempFile;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn write_kubeconfig(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().expect("NamedTempFile::new");
    f.write_all(content.as_bytes()).expect("write kubeconfig");
    f
}

fn source() -> Box<dyn beachcomber::provider::Source> {
    KubecontextProvider.sources().into_iter().next().unwrap()
}

// ── Metadata tests ─────────────────────────────────────────────────────────────

#[test]
fn source_is_path_scoped() {
    assert_eq!(source().metadata().scope, SourceScope::PathScoped);
}

#[test]
fn source_uses_watch_invalidation() {
    assert!(
        matches!(
            source().metadata().invalidation,
            InvalidationStrategy::Watch { .. }
        ),
        "expected Watch invalidation, got: {:?}",
        source().metadata().invalidation
    );
}

// ── execute(None) ──────────────────────────────────────────────────────────────

#[test]
fn execute_none_returns_empty() {
    let result = source().execute(None);
    assert!(
        result.fields.is_empty(),
        "execute(None) must return empty, got: {:?}",
        result.fields
    );
}

// ── Single-file execute ────────────────────────────────────────────────────────

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
    let result = source().execute(Some(f.path().to_str().unwrap()));

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
    let result = source().execute(Some("/tmp/beachcomber_test_nonexistent_kubeconfig_abc123"));
    assert!(
        result.fields.is_empty(),
        "missing file should produce empty result, got: {:?}",
        result.fields
    );
}

#[test]
fn malformed_kubeconfig_returns_failure() {
    let f = write_kubeconfig("not: valid: kubeconfig: garbage !!!\n\x00\x01\x02\n");
    let result = source().execute(Some(f.path().to_str().unwrap()));
    assert!(
        result.fields.is_empty(),
        "malformed kubeconfig should produce empty result, got: {:?}",
        result.fields
    );
}

#[test]
fn multiple_contexts_picks_current() {
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
    let result = source().execute(Some(f.path().to_str().unwrap()));

    assert_eq!(result.fields.get("context").unwrap().as_text(), "staging");
    assert_eq!(
        result.fields.get("namespace").unwrap().as_text(),
        "staging-ns"
    );
}

#[test]
fn empty_kubeconfig_returns_empty() {
    let f = write_kubeconfig("");
    let result = source().execute(Some(f.path().to_str().unwrap()));
    assert!(
        result.fields.is_empty(),
        "empty file should produce empty result, got: {:?}",
        result.fields
    );
}

#[test]
fn context_without_namespace_defaults_to_default() {
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
    let result = source().execute(Some(f.path().to_str().unwrap()));

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
    let yaml = "\
apiVersion: v1
kind: Config
current-context:
contexts: []
";
    let f = write_kubeconfig(yaml);
    let result = source().execute(Some(f.path().to_str().unwrap()));
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
    let result = source().execute(Some(f.path().to_str().unwrap()));

    assert!(
        matches!(result.fields.get("context"), Some(Value::String(_))),
        "context field must be Value::String"
    );
    assert!(
        matches!(result.fields.get("namespace"), Some(Value::String(_))),
        "namespace field must be Value::String"
    );
}

// ── Multi-file ':'-joined path ─────────────────────────────────────────────────

#[test]
fn multi_file_active_context_in_second_file() {
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
    let joined = format!(
        "{}:{}",
        f1.path().to_str().unwrap(),
        f2.path().to_str().unwrap()
    );
    let result = source().execute(Some(&joined));

    assert_eq!(
        result.fields.get("context").unwrap().as_text(),
        "dev",
        "current-context in second file must be honored"
    );
    assert_eq!(result.fields.get("namespace").unwrap().as_text(), "dev-ns");
}

#[test]
fn multi_file_context_namespace_from_different_file() {
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
    let joined = format!(
        "{}:{}",
        f1.path().to_str().unwrap(),
        f2.path().to_str().unwrap()
    );
    let result = source().execute(Some(&joined));

    assert_eq!(result.fields.get("context").unwrap().as_text(), "shared");
    assert_eq!(
        result.fields.get("namespace").unwrap().as_text(),
        "shared-ns",
        "namespace must be found even when context definition is in a different file"
    );
}

#[test]
fn name_match_not_a_substring() {
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
    let result = source().execute(Some(f.path().to_str().unwrap()));

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
    let joined = format!(
        "/tmp/beachcomber_test_nonexistent_s6_kube:{}",
        f.path().to_str().unwrap()
    );
    let result = source().execute(Some(&joined));

    assert_eq!(result.fields.get("context").unwrap().as_text(), "real");
}

// ── watched_files ─────────────────────────────────────────────────────────────

#[test]
fn watched_files_splits_colon_joined_path() {
    use std::path::PathBuf;
    let src = source();
    let files = src.watched_files(Some("/home/user/.kube/config:/etc/kube/extra.yaml"));
    assert_eq!(
        files,
        vec![
            PathBuf::from("/home/user/.kube/config"),
            PathBuf::from("/etc/kube/extra.yaml"),
        ]
    );
}

#[test]
fn watched_files_none_returns_empty() {
    let src = source();
    let files = src.watched_files(None);
    assert!(files.is_empty());
}

#[test]
fn watched_files_single_path() {
    use std::path::PathBuf;
    let src = source();
    let files = src.watched_files(Some("/home/user/.kube/config"));
    assert_eq!(files, vec![PathBuf::from("/home/user/.kube/config")]);
}
