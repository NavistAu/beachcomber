use beachcomber::provider::Provider;
use beachcomber::provider::Value;
use beachcomber::provider::mise::MiseProvider;
use tempfile::TempDir;

fn mise_toml_dir(tools: &[(&str, &str)]) -> TempDir {
    let d = tempfile::tempdir().unwrap();
    let mut body = String::from("[tools]\n");
    for (k, v) in tools {
        body.push_str(&format!("{k} = \"{v}\"\n"));
    }
    std::fs::write(d.path().join("mise.toml"), body).unwrap();
    d
}

#[test]
fn mise_canonical_path_returns_project_root_from_subdir() {
    let project = mise_toml_dir(&[("node", "20")]);
    let subdir = project.path().join("src").join("inner");
    std::fs::create_dir_all(&subdir).unwrap();

    let got = MiseProvider.canonical_path(Some(subdir.to_str().unwrap()));
    let expected = project.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn mise_canonical_path_returns_project_root_at_root() {
    let project = mise_toml_dir(&[("python", "3.12")]);
    let got = MiseProvider.canonical_path(Some(project.path().to_str().unwrap()));
    let expected = project.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn mise_canonical_path_finds_dot_mise_toml() {
    // Variant marker: `.mise.toml` (not `mise.toml`).
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join(".mise.toml"), "[tools]\n").unwrap();
    let subdir = d.path().join("sub");
    std::fs::create_dir_all(&subdir).unwrap();

    let got = MiseProvider.canonical_path(Some(subdir.to_str().unwrap()));
    let expected = d.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn mise_canonical_path_none_outside_project() {
    let d = tempfile::tempdir().unwrap();
    let got = MiseProvider.canonical_path(Some(d.path().to_str().unwrap()));
    // Either None (no upward project) or Some(ancestor with mise.toml).
    // The key invariant: never returns the bare tempdir itself.
    if let Some(got) = got {
        assert_ne!(
            got,
            d.path().to_string_lossy().to_string(),
            "tempdir has no mise.toml; canonical_path should not return the dir itself"
        );
    }
}

#[test]
fn mise_canonical_path_passes_none_through() {
    assert_eq!(MiseProvider.canonical_path(None), None);
}

#[test]
fn mise_metadata_has_pathscoped_sentinel() {
    use beachcomber::provider::{FieldScope, FieldType};
    let meta = MiseProvider.metadata();
    let sentinel = meta.fields.iter().find(|f| f.name == "<tool>").unwrap();
    assert!(matches!(sentinel.field_type, FieldType::String));
    assert!(matches!(sentinel.scope, FieldScope::PathScoped));
    // inferred_scope must be PathScoped so CWD-scoped queries route to the project entry.
    assert_eq!(meta.inferred_scope(), FieldScope::PathScoped);
}

#[test]
fn mise_execute_returns_flat_tool_fields() {
    let d = mise_toml_dir(&[("node", "20.11.0"), ("python", "3.12.1")]);

    // Trust the temp dir for mise so it parses our config (macOS-specific).
    let _ = std::process::Command::new("mise")
        .arg("trust")
        .arg(d.path())
        .output();

    let results = MiseProvider.execute(Some(d.path().to_str().unwrap()));
    // Skip if mise isn't installed or no project tools were returned.
    let Some((_, result)) = results.into_iter().find(|(p, _)| p.is_some()) else {
        return;
    };
    // Fields must be flat strings keyed by tool name, not wrapped in an object.
    let has_tool = result.get("node").is_some() || result.get("python").is_some();
    assert!(
        has_tool,
        "expected 'node' or 'python' as top-level string fields; got fields: {:?}",
        result.fields.keys().collect::<Vec<_>>()
    );
    let has_string = result
        .fields
        .values()
        .all(|v| matches!(v, Value::String(_)));
    assert!(has_string, "all tool values must be Value::String");
}

#[test]
fn mise_execute_none_emits_only_pathless_global_entry() {
    if std::process::Command::new("mise")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: mise binary not available");
        return;
    }

    let results = MiseProvider.execute(None);

    // execute(None) → exactly one pathless entry containing flat tool fields.
    assert_eq!(results.len(), 1, "expected 1 entry; got: {results:?}");
    assert!(results[0].0.is_none(), "entry must be pathless");
    assert!(
        results[0].1.fields.values().all(|v| matches!(v, Value::String(_))),
        "global entry must have flat Value::String tool fields; got: {:?}",
        results[0].1.fields
    );
}

#[test]
fn mise_execute_project_path_emits_only_path_scoped_entry() {
    if std::process::Command::new("mise")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: mise binary not available");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("mise.toml"), "[tools]\nrust = \"1.94\"\n").unwrap();

    // Trust the temp dir for mise so it parses our config (macOS-specific).
    let _ = std::process::Command::new("mise")
        .arg("trust")
        .arg(tmp.path())
        .output();

    let results = MiseProvider.execute(Some(tmp.path().to_str().unwrap()));

    // execute(Some(p)) → no pathless entry; one path-scoped entry with flat tool fields.
    assert!(
        results.iter().all(|(p, _)| p.is_some()),
        "project execution must not emit a pathless global entry; got: {results:?}"
    );
    let path_scoped = results.iter().find(|(p, _)| p.is_some());
    assert!(
        path_scoped.is_some(),
        "expected a path-scoped project entry; got: {results:?}"
    );
    let (_, project_result) = path_scoped.unwrap();
    assert!(
        project_result.fields.values().all(|v| matches!(v, Value::String(_))),
        "project entry fields must all be Value::String; got: {:?}",
        project_result.fields
    );
}

#[test]
fn mise_execute_project_path_no_config_emits_nothing() {
    let tmp = tempfile::tempdir().unwrap(); // no mise.toml
    let results = MiseProvider.execute(Some(tmp.path().to_str().unwrap()));
    assert!(
        results.is_empty(),
        "no mise.toml → execute should return empty; got: {results:?}"
    );
}
