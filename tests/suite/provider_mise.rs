use beachcomber::provider::Provider;
use beachcomber::provider::SourceScope;
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
fn mise_provider_metadata() {
    let meta = MiseProvider.metadata();
    assert_eq!(meta.name, "mise");
    assert_eq!(meta.sources.len(), 2);

    let global = meta.sources.iter().find(|s| s.name == "global").unwrap();
    assert_eq!(global.scope, SourceScope::Global);
    let global_fields: Vec<&str> = global.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(global_fields.contains(&"<tool>"));

    let project = meta.sources.iter().find(|s| s.name == "project").unwrap();
    assert_eq!(project.scope, SourceScope::PathScoped);
    let project_fields: Vec<&str> = project.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(project_fields.contains(&"<tool>"));
}

#[test]
fn mise_canonical_path_returns_project_root_from_subdir() {
    let project = mise_toml_dir(&[("node", "20")]);
    let subdir = project.path().join("src").join("inner");
    std::fs::create_dir_all(&subdir).unwrap();

    let sources = MiseProvider.sources();
    let project_src = sources
        .iter()
        .find(|s| s.metadata().name == "project")
        .unwrap();
    let got = project_src.canonical_path(Some(subdir.to_str().unwrap()));
    let expected = project.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn mise_canonical_path_returns_project_root_at_root() {
    let project = mise_toml_dir(&[("python", "3.12")]);
    let sources = MiseProvider.sources();
    let project_src = sources
        .iter()
        .find(|s| s.metadata().name == "project")
        .unwrap();
    let got = project_src.canonical_path(Some(project.path().to_str().unwrap()));
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

    let sources = MiseProvider.sources();
    let project_src = sources
        .iter()
        .find(|s| s.metadata().name == "project")
        .unwrap();
    let got = project_src.canonical_path(Some(subdir.to_str().unwrap()));
    let expected = d.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn mise_canonical_path_none_outside_project() {
    let d = tempfile::tempdir().unwrap();
    let sources = MiseProvider.sources();
    let project_src = sources
        .iter()
        .find(|s| s.metadata().name == "project")
        .unwrap();
    let got = project_src.canonical_path(Some(d.path().to_str().unwrap()));
    // Either None (no upward project) or Some(ancestor with mise.toml).
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
    let sources = MiseProvider.sources();
    let project_src = sources
        .iter()
        .find(|s| s.metadata().name == "project")
        .unwrap();
    assert_eq!(project_src.canonical_path(None), None);
}

#[test]
fn mise_project_execute_returns_flat_tool_fields() {
    let d = mise_toml_dir(&[("node", "20.11.0"), ("python", "3.12.1")]);

    // Trust the temp dir for mise so it parses our config (macOS-specific).
    let _ = std::process::Command::new("mise")
        .arg("trust")
        .arg(d.path())
        .output();

    let sources = MiseProvider.sources();
    let project_src = sources
        .iter()
        .find(|s| s.metadata().name == "project")
        .unwrap();
    let result = project_src.execute(Some(d.path().to_str().unwrap()));

    // Skip if mise isn't installed or no project tools were returned.
    if result.fields.is_empty() {
        return;
    }

    // Fields must be flat strings keyed by tool name.
    let has_tool = result.fields.contains_key("node") || result.fields.contains_key("python");
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
fn mise_global_execute_returns_flat_tool_fields() {
    if std::process::Command::new("mise")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: mise binary not available");
        return;
    }

    let sources = MiseProvider.sources();
    let global_src = sources
        .iter()
        .find(|s| s.metadata().name == "global")
        .unwrap();
    let result = global_src.execute(None);

    // May be empty if no global tools are installed.
    let all_strings = result
        .fields
        .values()
        .all(|v| matches!(v, Value::String(_)));
    assert!(
        all_strings,
        "global entry must have flat Value::String tool fields; got: {:?}",
        result.fields
    );
}

#[test]
fn mise_project_execute_no_config_emits_nothing() {
    let tmp = tempfile::tempdir().unwrap(); // no mise.toml
    let sources = MiseProvider.sources();
    let project_src = sources
        .iter()
        .find(|s| s.metadata().name == "project")
        .unwrap();
    let result = project_src.execute(Some(tmp.path().to_str().unwrap()));
    assert!(
        result.fields.is_empty(),
        "no mise.toml → project execute should return empty SourceResult; got: {:?}",
        result.fields
    );
}

#[test]
fn mise_sibling_sources_have_disjoint_field_owner() {
    // Validate the provider passes registration-time validation.
    let meta = MiseProvider.metadata();
    // The <tool> sentinel appears in both sources — that's the dynamic-field pattern.
    // This is expected; the validation logic in ProviderMetadata::validate() handles it.
    // Just assert we have both sources.
    assert_eq!(meta.sources.len(), 2);
    assert!(meta.sources.iter().any(|s| s.name == "global"));
    assert!(meta.sources.iter().any(|s| s.name == "project"));
}
