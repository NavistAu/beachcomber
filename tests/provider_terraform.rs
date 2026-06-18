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
fn terraform_workspace_from_environment_file() {
    let dir = make_tf_root(Some("staging"));
    let sources = TerraformProvider.sources();
    let result = sources[0].execute(Some(dir.path().to_str().unwrap()));
    assert_eq!(result.fields.get("workspace").unwrap().as_text(), "staging");
}

#[test]
fn terraform_tf_workspace_env_overrides_file() {
    let dir = make_tf_root(Some("from-file"));
    let result = temp_env::with_var("TF_WORKSPACE", Some("from-env"), || {
        let sources = TerraformProvider.sources();
        sources[0].execute(Some(dir.path().to_str().unwrap()))
    });
    assert_eq!(
        result.fields.get("workspace").unwrap().as_text(),
        "from-env",
        "$TF_WORKSPACE must override .terraform/environment"
    );
}

#[test]
fn terraform_no_environment_file_defaults_to_default() {
    let dir = make_tf_root(None); // .terraform exists but no environment file
    // Use temp_env to ensure TF_WORKSPACE is unset for this test.
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
