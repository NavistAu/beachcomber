/// Tests for the terraform provider.
/// Uses tempdir to isolate from the real filesystem.
use beachcomber::provider::Provider;
use beachcomber::provider::terraform::TerraformProvider;

fn make_tf_root(ws_name: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".terraform")).unwrap();
    if let Some(ws) = ws_name {
        std::fs::write(dir.path().join(".terraform").join("environment"), ws).unwrap();
    }
    dir
}

#[test]
fn terraform_workspace_field_present() {
    // Confirms that the daemon field is named 'workspace'.
    // This is the field the virtual cascade 'workspace = "env.TF_WORKSPACE or cache.terraform.workspace"' references.
    let dir = make_tf_root(Some("staging"));
    let sources = TerraformProvider.sources();
    let result = sources[0].execute(Some(dir.path().to_str().unwrap()));
    assert!(
        result.fields.contains_key("workspace"),
        "daemon field must be 'workspace'; got keys: {:?}",
        result.fields.keys().collect::<Vec<_>>()
    );
    assert!(
        !result.fields.contains_key("path_workspace"),
        "'path_workspace' key must be gone; found it in: {:?}",
        result.fields.keys().collect::<Vec<_>>()
    );
    assert_eq!(result.fields["workspace"].as_text(), "staging");
}

#[test]
fn terraform_no_env_read_for_tf_workspace() {
    // Confirms the daemon no longer reads $TF_WORKSPACE.
    let dir = make_tf_root(Some("from-file"));
    let result = temp_env::with_var("TF_WORKSPACE", Some("from-env"), || {
        let sources = TerraformProvider.sources();
        sources[0].execute(Some(dir.path().to_str().unwrap()))
    });
    // Daemon must return 'from-file' (file value), not 'from-env' (env).
    assert_eq!(
        result.fields.get("workspace").unwrap().as_text(),
        "from-file",
        "daemon must not read $TF_WORKSPACE; that is a client-side virtual field (expression form)"
    );
}

#[test]
fn terraform_no_environment_file_defaults_to_default() {
    let dir = make_tf_root(None); // .terraform exists but no environment file
    let result = temp_env::with_var("TF_WORKSPACE", None::<&str>, || {
        let sources = TerraformProvider.sources();
        sources[0].execute(Some(dir.path().to_str().unwrap()))
    });
    assert_eq!(
        result.fields.get("workspace").unwrap().as_text(),
        "default",
        "no environment file → default workspace"
    );
}

#[test]
fn terraform_returns_none_without_terraform_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let sources = TerraformProvider.sources();
    let result = sources[0].execute(Some(tmp.path().to_str().unwrap()));
    assert!(result.fields.is_empty());
}

#[test]
fn terraform_canonical_path_returns_module_root_from_subdir() {
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".terraform")).unwrap();
    let subdir = project.path().join("envs").join("dev");
    std::fs::create_dir_all(&subdir).unwrap();

    let sources = TerraformProvider.sources();
    let got = sources[0].canonical_path(Some(subdir.to_str().unwrap()));
    let expected = project.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn terraform_canonical_path_none_outside_project() {
    let tmp = tempfile::tempdir().unwrap();
    let sources = TerraformProvider.sources();
    let got = sources[0].canonical_path(Some(tmp.path().to_str().unwrap()));
    if let Some(got) = got {
        assert_ne!(got, tmp.path().to_string_lossy().to_string());
    }
}

#[test]
fn terraform_canonical_path_passes_none_through() {
    let sources = TerraformProvider.sources();
    assert_eq!(sources[0].canonical_path(None), None);
}
