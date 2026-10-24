use crate::provider::Provider;
use crate::provider::Source;
use crate::provider::Value;
use crate::provider::mise::{MiseProvider, MiseTools};
use std::sync::{Arc, Mutex};
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

fn tools_source() -> Box<dyn Source> {
    MiseProvider.sources().into_iter().next().unwrap()
}

fn home() -> String {
    crate::query::home_dir()
        .expect("$HOME is set")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn mise_provider_is_one_source_with_a_tool_sentinel() {
    let meta = MiseProvider.metadata();
    assert_eq!(meta.name, "mise");
    assert_eq!(meta.sources.len(), 1);
    let tools = &meta.sources[0];
    assert_eq!(tools.name, "tools");
    let fields: Vec<&str> = tools.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(fields, vec!["<tool>"]);
    assert!(meta.validate(true).is_ok());
    assert!(tools_source().has_root());
}

#[test]
fn mise_watches_its_files_under_the_root_and_its_config_dir_absolutely() {
    let meta = MiseProvider.metadata();
    match &meta.sources[0].invalidation {
        crate::provider::InvalidationStrategy::Watch {
            patterns,
            abs_paths,
        } => {
            assert_eq!(
                patterns,
                &vec!["mise.toml".to_string(), ".mise.toml".into()]
            );
            assert_eq!(abs_paths.len(), 1);
            assert!(abs_paths[0].ends_with("/mise"), "{abs_paths:?}");
        }
        other => panic!("expected Watch, got {other:?}"),
    }
}

#[test]
fn the_tools_source_roots_at_the_mise_toml_directory() {
    let project = mise_toml_dir(&[("node", "20")]);
    let expected = project.path().to_string_lossy().to_string();
    let src = tools_source();

    // From a subdirectory, and from the project itself.
    let subdir = project.path().join("src").join("inner");
    std::fs::create_dir_all(&subdir).unwrap();
    assert_eq!(
        src.root(subdir.to_str().unwrap()),
        Ok(Some(expected.clone()))
    );
    assert_eq!(
        src.root(project.path().to_str().unwrap()),
        Ok(Some(expected))
    );
}

#[test]
fn the_tools_source_roots_at_a_dot_mise_toml_directory() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join(".mise.toml"), "[tools]\n").unwrap();
    let subdir = d.path().join("sub");
    std::fs::create_dir_all(&subdir).unwrap();
    assert_eq!(
        tools_source().root(subdir.to_str().unwrap()),
        Ok(Some(d.path().to_string_lossy().to_string()))
    );
}

#[test]
fn the_tools_source_has_no_root_outside_a_project() {
    let d = tempfile::tempdir().unwrap();
    // Either no project above (the instance at `~`) or an ancestor's mise.toml,
    // never the empty directory itself.
    if let Some(got) = tools_source().root(d.path().to_str().unwrap()).unwrap() {
        assert_ne!(got, d.path().to_string_lossy().to_string());
    }
}

/// What `mise ls --current --json` says in a project directory: `node` set by
/// the project's own `mise.toml`, `go` by the global config.
fn canned_ls(project: &str) -> String {
    let global = format!("{}/config.toml", super::mise::global_config_dir_for_tests());
    serde_json::json!({
        "node": [{"version": "20.1.0", "source": {"path": format!("{project}/mise.toml")}}],
        "go": [{"version": "1.22.0", "source": {"path": global}}],
    })
    .to_string()
}

#[test]
fn one_source_answers_project_then_global() {
    let project = "/work/proj";
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = Arc::clone(&asked);
    let src = MiseTools::with_runner(Box::new(move |cwd: &str| {
        log.lock().unwrap().push(cwd.to_string());
        Ok(canned_ls(project))
    }));

    // At the project's instance: only what the project sets, run in the project.
    let at_project = src.execute(Some(project)).unwrap();
    assert_eq!(
        at_project.fields.keys().collect::<Vec<_>>(),
        vec!["node"],
        "the project instance reports the project's tools"
    );
    assert_eq!(
        at_project.fields.get("node"),
        Some(&Value::String("20.1.0".into()))
    );

    // At the instance at `~` (the daemon passes `$HOME`): only the global tools.
    let at_home = src.execute(Some(&home())).unwrap();
    assert_eq!(at_home.fields.keys().collect::<Vec<_>>(), vec!["go"]);
    assert_eq!(
        at_home.fields.get("go"),
        Some(&Value::String("1.22.0".into()))
    );

    assert_eq!(*asked.lock().unwrap(), vec![project.to_string(), home()]);
}

#[test]
fn the_home_instance_reports_the_global_config_and_a_config_file_in_home() {
    let home = home();
    let global = format!("{}/config.toml", super::mise::global_config_dir_for_tests());
    // The project entry for `python` comes first, so precedence cannot be
    // an accident of order.
    let ls = serde_json::json!({
        "node": [{"version": "18.0.0", "source": {"path": format!("{home}/mise.toml")}}],
        "python": [
            {"version": "3.12.0", "source": {"path": format!("{home}/.mise.toml")}},
            {"version": "3.11.0", "source": {"path": global.clone()}},
        ],
        "go": [{"version": "1.22.0", "source": {"path": global}}],
        "ruby": [{"version": "3.3.0", "source": {"path": "/work/proj/mise.toml"}}],
    })
    .to_string();
    let src = MiseTools::with_runner(Box::new(move |_| Ok(ls.clone())));

    let got = src.execute(Some(&home)).unwrap().fields;
    let mut keys: Vec<_> = got.keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["go", "node", "python"],
        "ruby is another directory's"
    );
    assert_eq!(got.get("node"), Some(&Value::String("18.0.0".into())));
    assert_eq!(
        got.get("python"),
        Some(&Value::String("3.12.0".into())),
        "the project config wins a clash"
    );
}

#[test]
fn a_tools_run_that_fails_is_a_failure_not_an_empty_result() {
    let failing = MiseTools::with_runner(Box::new(|_| Err("mise: not found".into())));
    assert_eq!(
        failing.execute(Some("/work/proj")).unwrap_err(),
        "mise: not found"
    );
    let garbled = MiseTools::with_runner(Box::new(|_| Ok("not json".into())));
    assert!(garbled.execute(Some("/work/proj")).is_err());
    assert!(
        MiseTools::with_runner(Box::new(|_| Ok("{}".into())))
            .execute(None)
            .is_err(),
        "no directory to run in is a failure"
    );
}

#[test]
fn a_project_with_no_tools_is_an_empty_result() {
    let src = MiseTools::with_runner(Box::new(|_| Ok("{}".into())));
    assert!(src.execute(Some("/work/proj")).unwrap().fields.is_empty());
}

#[test]
fn mise_project_execute_returns_flat_tool_fields() {
    let d = mise_toml_dir(&[("node", "20.11.0"), ("python", "3.12.1")]);
    let project = d.path().to_str().unwrap();

    // The shape of a project's answer, from mise's own output format.
    let ls = serde_json::json!({
        "node": [{"version": "20.11.0", "source": {"path": format!("{project}/mise.toml")}}],
        "python": [{"version": "3.12.1", "source": {"path": format!("{project}/mise.toml")}}],
    })
    .to_string();
    let result = MiseTools::with_runner(Box::new(move |_| Ok(ls.clone())))
        .execute(Some(project))
        .unwrap();
    assert_eq!(
        result.fields.get("node"),
        Some(&Value::String("20.11.0".into()))
    );
    assert!(
        result
            .fields
            .values()
            .all(|v| matches!(v, Value::String(_))),
        "all tool values must be Value::String"
    );
}

#[test]
fn mise_project_execute_with_the_real_binary_reports_the_project() {
    if std::process::Command::new("mise")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: mise binary not available");
        return;
    }
    let d = mise_toml_dir(&[("node", "20.11.0"), ("python", "3.12.1")]);
    // Trust the temp dir for mise so it parses our config.
    let _ = std::process::Command::new("mise")
        .arg("trust")
        .arg(d.path())
        .output();

    let result = tools_source()
        .execute(Some(d.path().to_str().unwrap()))
        .expect("mise is installed, so the run succeeds");
    assert!(
        result.fields.contains_key("node") && result.fields.contains_key("python"),
        "the project's tools: {:?}",
        result.fields.keys().collect::<Vec<_>>()
    );
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

    // The instance at `~` runs in `$HOME`.
    let result = tools_source()
        .execute(Some(&home()))
        .expect("mise is installed, so the run succeeds");

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
