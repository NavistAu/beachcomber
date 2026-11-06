//! `defaults/python.toml`: the `venv` Source, one instance per Python project
//! (the nearest directory with a `pyproject.toml` or a virtualenv), answering
//! alone, and the `version` and `venv_name` templates over it.

use super::{age, scenario, schedule, served};
use beachcomber::provider::{FailbackConfig, InvalidationStrategy};
use serde_json::json;
use std::path::Path;

/// A virtualenv `name` in `dir` whose `pyvenv.cfg` holds `cfg`.
fn venv(dir: &Path, name: &str, cfg: &str) {
    let venv = dir.join(name);
    std::fs::create_dir_all(venv.join("bin")).unwrap();
    std::fs::write(venv.join("pyvenv.cfg"), cfg).unwrap();
}

/// The fields are `venv`, `local_venv_name` and `venv_version`, never
/// `version` (the template's name).
#[test]
fn python_field_is_venv_version_not_version() {
    let s = served(&[], "");
    venv(&s.project, ".venv", "version = 3.12.1\nhome = /usr/bin\n");
    assert_eq!(
        s.get("python", Some(&s.project)),
        Some(json!({"venv": true, "local_venv_name": ".venv", "venv_version": "3.12.1"}))
    );
}

/// `local_venv_name` is the virtualenv directory's own name.
#[test]
fn python_local_venv_name_field_present() {
    let s = served(&[], "");
    venv(&s.project, ".venv", "version = 3.11.0\n");
    assert_eq!(
        s.get("python.local_venv_name", Some(&s.project)),
        Some(json!(".venv"))
    );
}

/// The daemon never reads `$VIRTUAL_ENV`: a project with no virtualenv on
/// disk has none, whatever the daemon's environment says.
#[test]
fn python_no_virtual_env_fallback() {
    let s = served(&[("VIRTUAL_ENV", "/some/external/venv")], "");
    std::fs::write(s.project.join("pyproject.toml"), "[project]\n").unwrap();
    assert_eq!(s.get("python", Some(&s.project)), None);
}

/// `venv` is a bool, `true`, for any of the four names.
#[test]
fn python_venv_bool_true_when_venv_found() {
    let s = served(&[], "");
    venv(&s.project, "venv", "version = 3.10.0\n");
    assert_eq!(s.get("python.venv", Some(&s.project)), Some(json!(true)));
}

/// virtualenv writes `version_info` and no `version`: the version is absent,
/// not taken from a key that starts the same.
#[test]
fn python_venv_version_exact_key_match_no_version_info_bleed() {
    let s = served(&[], "");
    venv(
        &s.project,
        ".venv",
        "version_info = 3.12.1.final.0\nhome = /usr/bin\n",
    );
    assert_eq!(
        s.get("python", Some(&s.project)),
        Some(json!({"venv": true, "local_venv_name": ".venv"}))
    );
}

/// The stdlib's `version = X.Y.Z`, wherever it stands in the file and however
/// it is spaced; a `version` with no value is passed over for one with a value.
#[test]
fn python_venv_version_bare_version_key_returns_value() {
    let s = served(&[], "");
    venv(
        &s.project,
        ".venv",
        "home = /usr/bin\nversion =\n  version=3.12.1  \r\nversion = 9\n",
    );
    assert_eq!(
        s.get("python.venv_version", Some(&s.project)),
        Some(json!("3.12.1"))
    );
}

/// The four names are tried in order, `.venv` first, and a name without a
/// `pyvenv.cfg` is no virtualenv.
#[test]
fn python_the_four_venv_names_in_order() {
    let s = served(&[], "");
    std::fs::create_dir_all(s.project.join(".venv")).unwrap();
    venv(&s.project, "env", "version = 3.9.0\n");
    venv(&s.project, ".virtualenv", "version = 3.10.0\n");
    assert_eq!(
        s.get("python.local_venv_name", Some(&s.project)),
        Some(json!(".virtualenv")),
        "an empty .venv is passed over; .virtualenv comes before env"
    );
    venv(&s.project, "venv", "version = 3.11.0\n");
    venv(&s.project, ".venv", "version = 3.12.0\n");
    assert_eq!(
        s.get("python", Some(&s.project)),
        Some(json!({"venv": true, "local_venv_name": ".venv", "venv_version": "3.12.0"}))
    );
}

/// `pyproject.toml` alone is a project with no virtualenv: a root, and
/// nothing to say.
#[test]
fn python_returns_none_without_venv() {
    let s = served(&[], "");
    std::fs::write(s.project.join("pyproject.toml"), "[project]\n").unwrap();
    assert_eq!(s.get("python", Some(&s.project)), None);
    s.ran_at("python", Some(&s.project), "venv");
}

/// A directory below a `pyproject.toml` reads that project's instance.
#[test]
fn python_root_returns_pyproject_dir_from_subdir() {
    let s = served(&[], "");
    std::fs::write(s.project.join("pyproject.toml"), "[project]\n").unwrap();
    venv(&s.project, ".venv", "version = 3.12.1\n");
    let sub = s.project.join("src/pkg");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(
        s.get("python.venv_version", Some(&sub)),
        Some(json!("3.12.1"))
    );
    s.ran_at("python", Some(&s.project), "venv");
}

/// A virtualenv alone, with no `pyproject.toml`, makes its directory the
/// project.
#[test]
fn python_root_returns_venv_dir_from_subdir() {
    let s = served(&[], "");
    venv(&s.project, ".venv", "version = 3.12\n");
    let sub = s.project.join("nested");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(
        s.get("python.venv_version", Some(&sub)),
        Some(json!("3.12"))
    );
    s.ran_at("python", Some(&s.project), "venv");
}

/// A directory in no project has no instance.
#[test]
fn python_root_none_outside_project() {
    let s = served(&[], "");
    assert_eq!(s.get("python", Some(&s.project)), None);
    assert_eq!(s.get("python", Some(&s.home)), None);
    assert_eq!(s.get("python", None), None);
}

/// A project nested in another answers alone: its own virtualenv, or none,
/// never the outer one's.
#[test]
fn python_nested_projects_answer_the_nearest() {
    let s = served(&[], "");
    venv(&s.project, ".venv", "version = 3.12.0\n");
    let inner = s.project.join("tools/inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("pyproject.toml"), "[project]\n").unwrap();
    assert_eq!(s.get("python", Some(&inner)), None);
    venv(&inner, "venv", "version = 3.13.0\n");
    assert_eq!(
        s.get("python", Some(&inner)),
        Some(json!({"venv": true, "local_venv_name": "venv", "venv_version": "3.13.0"}))
    );
    assert_eq!(
        s.get("python.venv_version", Some(&s.project)),
        Some(json!("3.12.0"))
    );
}

/// With nothing watching and nothing polling, a rewritten `pyvenv.cfg` is
/// seen by the next read, and a read with nothing changed runs nothing.
#[test]
fn python_a_rewritten_pyvenv_cfg_is_seen_by_the_next_read() {
    let s = served(&[], "");
    venv(&s.project, ".venv", "version = 3.12.0\n");
    let cfg = s.project.join(".venv/pyvenv.cfg");
    age(&cfg);
    age(&s.project.join(".venv"));
    assert_eq!(
        s.get("python.venv_version", Some(&s.project)),
        Some(json!("3.12.0"))
    );
    let ran = s.ran_at("python", Some(&s.project), "venv");
    for _ in 0..5 {
        s.get("python.venv_version", Some(&s.project));
    }
    assert_eq!(s.ran_at("python", Some(&s.project), "venv"), ran);
    std::fs::write(&cfg, "version = 3.13.1\n").unwrap();
    assert_eq!(
        s.get("python.venv_version", Some(&s.project)),
        Some(json!("3.13.1"))
    );
}

/// The templates render over the Source: `version` falls through the shell
/// and the tool managers to the virtualenv's, and `venv_name` is the local
/// virtualenv's, before `$VIRTUAL_ENV`.
#[test]
fn python_the_templates_render_over_the_source() {
    // `python.version` reads `comb.mise.python` first: the daemon's `PATH`
    // has no `mise`, so the developer's own never runs (it would take most of
    // a second, and answer from its own config).
    // SAFETY: nextest runs each test in its own process, and the daemon that
    // reads this `PATH` has not started.
    unsafe { std::env::set_var("PATH", "/usr/bin:/bin") };
    let s = scenario();
    venv(&s.project, ".venv", "version = 3.12.4\n");
    assert_eq!(s.get("python.version", &s.project), Some(json!("3.12.4")));
    assert_eq!(
        s.get_in_env(
            "python.venv_name",
            &s.project,
            &[("VIRTUAL_ENV", "/x/other")]
        ),
        Some(json!(".venv"))
    );
    assert_eq!(
        s.get_in_env("python.version", &s.project, &[("PYENV_VERSION", "3.9.1")]),
        Some(json!("3.9.1"))
    );
}

/// The Rust provider's schedule, kept: watch `.venv` and `pyproject.toml`,
/// retry a failure every minute three times, the nearest project alone.
#[test]
fn python_keeps_its_schedule() {
    let (meta, rooted) = schedule("python", "venv");
    assert!(rooted);
    assert_eq!(
        meta.invalidation,
        InvalidationStrategy::Watch {
            patterns: vec![".venv".into(), "pyproject.toml".into()],
            abs_paths: vec![],
        }
    );
    assert_eq!(
        meta.failback,
        FailbackConfig {
            reattempts: 3,
            interval_secs: 60
        }
    );
    assert!(meta.nearest);
    assert_eq!(meta.decay, None, "the provider's decay prototype");
}

/// A shipped Source takes any line from a later file, one leaf at a time:
/// here `result.venv_version`, while `venv` and `local_venv_name` stay as
/// shipped.
#[test]
fn python_a_later_file_sets_one_result_line() {
    let s = served(
        &[],
        "[daemon.providers.python.sources.venv]\n\
         result.venv_version = \"{{ 'py' ~ ((data.cfg | regex_search('version = (.*)'))[1]) }}\"\n",
    );
    venv(&s.project, ".venv", "version = 3.12.1\n");
    assert_eq!(
        s.get("python", Some(&s.project)),
        Some(json!({"venv": true, "local_venv_name": ".venv", "venv_version": "py3.12.1"}))
    );
}
