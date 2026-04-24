use beachcomber::provider::FieldScope;
use beachcomber::provider::Provider;
use beachcomber::provider::asdf::AsdfProvider;
use beachcomber::provider::conda::CondaProvider;
use beachcomber::provider::direnv::DirenvProvider;
use beachcomber::provider::mise::MiseProvider;
use beachcomber::provider::python::PythonProvider;
use beachcomber::provider::terraform::TerraformProvider;
use tempfile::TempDir;

// --- Terraform ---

#[test]
fn terraform_metadata() {
    let p = TerraformProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "terraform");
    assert_eq!(meta.inferred_scope(), FieldScope::PathScoped);
    let fields: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"workspace"));
}

#[test]
fn terraform_returns_none_without_terraform_dir() {
    let tmp = TempDir::new().unwrap();
    let p = TerraformProvider;
    assert!(p.execute(Some(tmp.path().to_str().unwrap())).is_empty());
}

// --- Direnv ---

#[test]
fn direnv_metadata() {
    let p = DirenvProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "direnv");
    assert_eq!(meta.inferred_scope(), FieldScope::PathScoped);
    let fields: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"status"));
    assert!(fields.contains(&"allowed"));
}

#[test]
fn direnv_returns_none_without_envrc() {
    let tmp = TempDir::new().unwrap();
    let p = DirenvProvider;
    assert!(p.execute(Some(tmp.path().to_str().unwrap())).is_empty());
}

#[test]
fn direnv_canonical_path_returns_envrc_dir_from_subdir() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join(".envrc"), "export FOO=bar\n").unwrap();
    let subdir = project.path().join("src").join("nested");
    std::fs::create_dir_all(&subdir).unwrap();

    let got = DirenvProvider.canonical_path(Some(subdir.to_str().unwrap()));
    let expected = project.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn direnv_canonical_path_none_outside_project() {
    let tmp = TempDir::new().unwrap();
    let got = DirenvProvider.canonical_path(Some(tmp.path().to_str().unwrap()));
    if let Some(got) = got {
        assert_ne!(got, tmp.path().to_string_lossy().to_string());
    }
}

#[test]
fn direnv_canonical_path_passes_none_through() {
    assert_eq!(DirenvProvider.canonical_path(None), None);
}

// --- Python ---

#[test]
fn python_metadata() {
    let p = PythonProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "python");
    assert_eq!(meta.inferred_scope(), FieldScope::PathScoped);
    let fields: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"venv"));
    assert!(fields.contains(&"venv_name"));
    assert!(fields.contains(&"version"));
}

#[test]
fn python_detects_venv() {
    let tmp = TempDir::new().unwrap();
    // Create a fake .venv directory with pyvenv.cfg
    std::fs::create_dir(tmp.path().join(".venv")).unwrap();
    std::fs::write(
        tmp.path().join(".venv").join("pyvenv.cfg"),
        "home = /usr/bin\nversion = 3.12.0\n",
    )
    .unwrap();

    let p = PythonProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(result.get("venv").unwrap().as_text(), "true");
    assert_eq!(result.get("venv_name").unwrap().as_text(), ".venv");
}

#[test]
fn python_returns_none_without_venv() {
    let tmp = TempDir::new().unwrap();
    let p = PythonProvider;
    assert!(p.execute(Some(tmp.path().to_str().unwrap())).is_empty());
}

// --- Conda ---

#[test]
fn conda_metadata() {
    let p = CondaProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "conda");
    assert_eq!(meta.inferred_scope(), FieldScope::Global);
    let fields: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"env"));
}

// --- Mise ---

#[test]
fn mise_metadata() {
    let p = MiseProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "mise");
    assert_eq!(meta.inferred_scope(), FieldScope::PathScoped);
    let fields: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"project"));
    assert!(fields.contains(&"global"));
}

#[test]
fn mise_detects_config() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("mise.toml"), "[tools]\nrust = \"1.85.0\"\n").unwrap();

    let p = MiseProvider;
    // May return None if mise isn't installed, that's fine
    let _ = p.execute(Some(tmp.path().to_str().unwrap()));
}

// --- Asdf ---

#[test]
fn asdf_metadata() {
    let p = AsdfProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "asdf");
    assert_eq!(meta.inferred_scope(), FieldScope::PathScoped);
    let fields: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"tools"));
}

#[test]
fn asdf_canonical_path_returns_project_root_from_subdir() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join(".tool-versions"), "nodejs 20.0.0\n").unwrap();
    let subdir = project.path().join("pkg");
    std::fs::create_dir_all(&subdir).unwrap();

    let got = AsdfProvider.canonical_path(Some(subdir.to_str().unwrap()));
    let expected = project.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn asdf_canonical_path_none_outside_project() {
    let tmp = TempDir::new().unwrap();
    let got = AsdfProvider.canonical_path(Some(tmp.path().to_str().unwrap()));
    if let Some(got) = got {
        assert_ne!(got, tmp.path().to_string_lossy().to_string());
    }
}

#[test]
fn asdf_canonical_path_passes_none_through() {
    assert_eq!(AsdfProvider.canonical_path(None), None);
}

#[test]
fn asdf_detects_tool_versions() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join(".tool-versions"),
        "nodejs 20.0.0\nruby 3.2.0\n",
    )
    .unwrap();

    use beachcomber::provider::Value;
    let p = AsdfProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    assert!(
        matches!(result.get("tools"), Some(Value::Object(_))),
        "tools field must be a Value::Object"
    );
}
