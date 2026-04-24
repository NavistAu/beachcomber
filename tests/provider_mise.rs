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
fn mise_tools_field_type_is_object() {
    use beachcomber::provider::FieldType;
    let meta = MiseProvider.metadata();
    let project = meta.fields.iter().find(|f| f.name == "project").unwrap();
    let global = meta.fields.iter().find(|f| f.name == "global").unwrap();
    assert!(matches!(project.field_type, FieldType::Object));
    assert!(matches!(global.field_type, FieldType::Object));
}

#[test]
fn mise_execute_returns_object_map() {
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
    match result.get("project") {
        Some(Value::Object(map)) => {
            assert!(
                map.contains_key("node") || map.contains_key("python"),
                "expected node or python in project map; got: {map:?}"
            );
        }
        _ => panic!("project must be a Value::Object"),
    }
}

#[test]
fn mise_emits_global_pathless_and_project_path_scoped() {
    use beachcomber::provider::Provider;
    use beachcomber::provider::mise::MiseProvider;

    // Skip if mise binary is unavailable.
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

    let prov = MiseProvider;
    let results = prov.execute(Some(tmp.path().to_str().unwrap()));

    let pathless = results.iter().find(|(p, _)| p.is_none());
    assert!(
        pathless.is_some(),
        "mise should emit a pathless global entry; got: {results:?}"
    );
    let (_, global_result) = pathless.unwrap();
    assert!(
        global_result.get("global").is_some(),
        "global entry should contain 'global' field"
    );

    let path_scoped = results.iter().find(|(p, _)| p.is_some());
    // Project entry may be empty if the temp dir isn't trusted — but the entry should exist.
    assert!(
        path_scoped.is_some(),
        "mise should emit a path-scoped project entry when mise.toml exists; got: {results:?}"
    );
    let (_, project_result) = path_scoped.unwrap();
    assert!(
        project_result.get("project").is_some(),
        "project entry should contain 'project' field"
    );
}

#[test]
fn mise_emits_only_global_when_no_project_config() {
    use beachcomber::provider::Provider;
    use beachcomber::provider::mise::MiseProvider;

    if std::process::Command::new("mise")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: mise binary not available");
        return;
    }

    let tmp = tempfile::tempdir().unwrap(); // no mise.toml

    let prov = MiseProvider;
    let results = prov.execute(Some(tmp.path().to_str().unwrap()));

    // Only the global entry — no project entry because no mise.toml in path.
    assert_eq!(
        results.len(),
        1,
        "expected 1 entry (global only); got: {results:?}"
    );
    assert!(
        results[0].0.is_none(),
        "expected the one entry to be pathless"
    );
    assert!(
        results[0].1.get("global").is_some(),
        "global field must be present"
    );
}
