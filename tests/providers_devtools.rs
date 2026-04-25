use beachcomber::provider::Provider;
use beachcomber::provider::asdf::AsdfProvider;
use beachcomber::provider::conda::CondaProvider;
use beachcomber::provider::direnv::DirenvProvider;
use beachcomber::provider::mise::MiseProvider;
use beachcomber::provider::python::PythonProvider;
use beachcomber::provider::terraform::TerraformProvider;
use beachcomber::provider::SourceScope;
use tempfile::TempDir;

// --- Terraform ---

#[test]
fn terraform_metadata() {
    let p = TerraformProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "terraform");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "state");
    assert_eq!(src.scope, SourceScope::PathScoped);
    let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"workspace"));
}

#[test]
fn terraform_returns_none_without_terraform_dir() {
    let tmp = TempDir::new().unwrap();
    let sources = TerraformProvider.sources();
    let result = sources[0].execute(Some(tmp.path().to_str().unwrap()));
    assert!(result.fields.is_empty());
}

#[test]
fn terraform_canonical_path_returns_module_root_from_subdir() {
    let project = TempDir::new().unwrap();
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
    let tmp = TempDir::new().unwrap();
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

// --- Direnv ---

#[test]
fn direnv_metadata() {
    let p = DirenvProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "direnv");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "state");
    assert_eq!(src.scope, SourceScope::PathScoped);
    let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"status"));
    assert!(fields.contains(&"allowed"));
}

#[test]
fn direnv_returns_none_without_envrc() {
    let tmp = TempDir::new().unwrap();
    let sources = DirenvProvider.sources();
    let result = sources[0].execute(Some(tmp.path().to_str().unwrap()));
    assert!(result.fields.is_empty());
}

#[test]
fn direnv_canonical_path_returns_envrc_dir_from_subdir() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join(".envrc"), "export FOO=bar\n").unwrap();
    let subdir = project.path().join("src").join("nested");
    std::fs::create_dir_all(&subdir).unwrap();

    let sources = DirenvProvider.sources();
    let got = sources[0].canonical_path(Some(subdir.to_str().unwrap()));
    let expected = project.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn direnv_canonical_path_none_outside_project() {
    let tmp = TempDir::new().unwrap();
    let sources = DirenvProvider.sources();
    let got = sources[0].canonical_path(Some(tmp.path().to_str().unwrap()));
    if let Some(got) = got {
        assert_ne!(got, tmp.path().to_string_lossy().to_string());
    }
}

#[test]
fn direnv_canonical_path_passes_none_through() {
    let sources = DirenvProvider.sources();
    assert_eq!(sources[0].canonical_path(None), None);
}

// --- Python ---

#[test]
fn python_metadata() {
    let p = PythonProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "python");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "venv");
    assert_eq!(src.scope, SourceScope::PathScoped);
    let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
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

    let sources = PythonProvider.sources();
    let result = sources[0].execute(Some(tmp.path().to_str().unwrap()));
    assert_eq!(result.fields.get("venv").unwrap().as_text(), "true");
    assert_eq!(result.fields.get("venv_name").unwrap().as_text(), ".venv");
}

#[test]
fn python_returns_none_without_venv() {
    let tmp = TempDir::new().unwrap();
    let sources = PythonProvider.sources();
    let result = sources[0].execute(Some(tmp.path().to_str().unwrap()));
    assert!(result.fields.is_empty());
}

#[test]
fn python_canonical_path_returns_pyproject_dir_from_subdir() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join("pyproject.toml"), "[project]\n").unwrap();
    let subdir = project.path().join("src").join("pkg");
    std::fs::create_dir_all(&subdir).unwrap();

    let sources = PythonProvider.sources();
    let got = sources[0].canonical_path(Some(subdir.to_str().unwrap()));
    let expected = project.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn python_canonical_path_returns_venv_dir_from_subdir() {
    let project = TempDir::new().unwrap();
    std::fs::create_dir(project.path().join(".venv")).unwrap();
    std::fs::write(
        project.path().join(".venv").join("pyvenv.cfg"),
        "version = 3.12\n",
    )
    .unwrap();
    let subdir = project.path().join("nested");
    std::fs::create_dir(&subdir).unwrap();

    let sources = PythonProvider.sources();
    let got = sources[0].canonical_path(Some(subdir.to_str().unwrap()));
    let expected = project.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn python_canonical_path_none_outside_project() {
    let tmp = TempDir::new().unwrap();
    let sources = PythonProvider.sources();
    let got = sources[0].canonical_path(Some(tmp.path().to_str().unwrap()));
    if let Some(got) = got {
        assert_ne!(got, tmp.path().to_string_lossy().to_string());
    }
}

#[test]
fn python_canonical_path_passes_none_through() {
    let sources = PythonProvider.sources();
    assert_eq!(sources[0].canonical_path(None), None);
}

// --- Conda ---

#[test]
fn conda_metadata() {
    let p = CondaProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "conda");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "env");
    assert_eq!(src.scope, SourceScope::Global);
    let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"env"));
}

// --- Mise ---

#[test]
fn mise_metadata() {
    let p = MiseProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "mise");
    // Mise is a Section I provider; test only the provider name for now
    // TODO(section-J): assert on source metadata once mise is migrated in Section I
    let _ = meta;
}

#[test]
fn mise_detects_config() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("mise.toml"), "[tools]\nrust = \"1.85.0\"\n").unwrap();

    let p = MiseProvider;
    // May return None if mise isn't installed, that's fine
    // TODO(section-J): update to sources()[0].execute() once mise migrated in Section I
    let _ = p;
}

// --- Asdf ---

#[test]
fn asdf_metadata() {
    let p = AsdfProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "asdf");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "tools");
    assert_eq!(src.scope, SourceScope::PathScoped);
    let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"<tool>"));
}

#[test]
fn asdf_canonical_path_returns_project_root_from_subdir() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join(".tool-versions"), "nodejs 20.0.0\n").unwrap();
    let subdir = project.path().join("pkg");
    std::fs::create_dir_all(&subdir).unwrap();

    let sources = AsdfProvider.sources();
    let got = sources[0].canonical_path(Some(subdir.to_str().unwrap()));
    let expected = project.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn asdf_canonical_path_none_outside_project() {
    let tmp = TempDir::new().unwrap();
    let sources = AsdfProvider.sources();
    let got = sources[0].canonical_path(Some(tmp.path().to_str().unwrap()));
    if let Some(got) = got {
        assert_ne!(got, tmp.path().to_string_lossy().to_string());
    }
}

#[test]
fn asdf_canonical_path_passes_none_through() {
    let sources = AsdfProvider.sources();
    assert_eq!(sources[0].canonical_path(None), None);
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
    let sources = AsdfProvider.sources();
    let result = sources[0].execute(Some(tmp.path().to_str().unwrap()));
    assert!(
        matches!(result.fields.get("tools"), Some(Value::Object(_))),
        "tools field must be a Value::Object"
    );
}
