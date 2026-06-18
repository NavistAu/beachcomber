use beachcomber::provider::Provider;
use beachcomber::provider::python::PythonProvider;
use tempfile::TempDir;

fn make_venv(name: &str, version: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let venv = dir.path().join(name);
    std::fs::create_dir_all(venv.join("bin")).unwrap();
    let cfg = format!("version = {version}\nhome = /usr/bin\n");
    std::fs::write(venv.join("pyvenv.cfg"), cfg).unwrap();
    dir
}

#[test]
fn python_field_is_venv_version_not_version() {
    // Confirms the daemon field is 'venv_version', not 'version'.
    let dir = make_venv(".venv", "3.12.1");
    let sources = PythonProvider.sources();
    let result = sources[0].execute(Some(dir.path().to_str().unwrap()));
    assert!(
        result.fields.contains_key("venv_version"),
        "field must be 'venv_version'; got: {:?}",
        result.fields.keys().collect::<Vec<_>>()
    );
    assert!(
        !result.fields.contains_key("version"),
        "old 'version' key must be absent"
    );
    assert_eq!(result.fields["venv_version"].as_text(), "3.12.1");
}

#[test]
fn python_local_venv_name_field_present() {
    // The new 'local_venv_name' field holds the raw directory name (.venv, venv, etc.).
    let dir = make_venv(".venv", "3.11.0");
    let sources = PythonProvider.sources();
    let result = sources[0].execute(Some(dir.path().to_str().unwrap()));
    assert_eq!(result.fields["local_venv_name"].as_text(), ".venv");
}

#[test]
fn python_no_virtual_env_fallback() {
    // Confirms the daemon ignores $VIRTUAL_ENV.
    let dir = TempDir::new().unwrap(); // no venv on disk
    let result = temp_env::with_var("VIRTUAL_ENV", Some("/some/external/venv"), || {
        let sources = PythonProvider.sources();
        sources[0].execute(Some(dir.path().to_str().unwrap()))
    });
    assert!(
        result.fields.is_empty(),
        "daemon must not fall back to $VIRTUAL_ENV; got: {:?}",
        result.fields
    );
}

#[test]
fn python_venv_bool_true_when_venv_found() {
    let dir = make_venv("venv", "3.10.0");
    let sources = PythonProvider.sources();
    let result = sources[0].execute(Some(dir.path().to_str().unwrap()));
    assert_eq!(
        result.fields["venv"].as_text(),
        "true",
        "venv field must be bool true"
    );
}
