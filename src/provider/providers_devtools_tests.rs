use crate::provider::Provider;
use crate::provider::asdf::AsdfProvider;
use crate::provider::direnv::DirenvProvider;
use tempfile::TempDir;

// --- Direnv ---

#[test]
fn direnv_metadata() {
    let p = DirenvProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "direnv");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "state");
    let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"status"));
    assert!(fields.contains(&"allowed"));
}

#[test]
fn direnv_returns_none_without_envrc() {
    let tmp = TempDir::new().unwrap();
    let sources = DirenvProvider.sources();
    let result = sources[0]
        .execute(Some(tmp.path().to_str().unwrap()))
        .unwrap();
    assert!(result.fields.is_empty());
}

#[test]
fn direnv_root_returns_envrc_dir_from_subdir() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join(".envrc"), "export FOO=bar\n").unwrap();
    let subdir = project.path().join("src").join("nested");
    std::fs::create_dir_all(&subdir).unwrap();

    let sources = DirenvProvider.sources();
    let got = sources[0].root(subdir.to_str().unwrap()).unwrap();
    let expected = project.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn direnv_root_none_outside_project() {
    let tmp = TempDir::new().unwrap();
    let sources = DirenvProvider.sources();
    let got = sources[0].root(tmp.path().to_str().unwrap()).unwrap();
    if let Some(got) = got {
        assert_ne!(got, tmp.path().to_string_lossy().to_string());
    }
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
    let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"<tool>"));
}

#[test]
fn asdf_root_returns_project_root_from_subdir() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join(".tool-versions"), "nodejs 20.0.0\n").unwrap();
    let subdir = project.path().join("pkg");
    std::fs::create_dir_all(&subdir).unwrap();

    let sources = AsdfProvider.sources();
    let got = sources[0].root(subdir.to_str().unwrap()).unwrap();
    let expected = project.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn asdf_root_none_outside_project() {
    let tmp = TempDir::new().unwrap();
    let sources = AsdfProvider.sources();
    let got = sources[0].root(tmp.path().to_str().unwrap()).unwrap();
    if let Some(got) = got {
        assert_ne!(got, tmp.path().to_string_lossy().to_string());
    }
}

#[test]
fn asdf_detects_tool_versions() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join(".tool-versions"),
        "nodejs 20.0.0\nruby 3.2.0\n",
    )
    .unwrap();

    let sources = AsdfProvider.sources();
    let result = sources[0]
        .execute(Some(tmp.path().to_str().unwrap()))
        .unwrap();
    // Each tool is a flat top-level field (not nested under "tools" Object).
    assert!(
        !result.fields.contains_key("tools"),
        "tools must not be a nested Object; got keys: {:?}",
        result.fields.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        result.fields.get("nodejs").unwrap().as_text(),
        "20.0.0",
        "nodejs should be a flat field"
    );
    assert_eq!(
        result.fields.get("ruby").unwrap().as_text(),
        "3.2.0",
        "ruby should be a flat field"
    );
}
