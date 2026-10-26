//! `defaults/terraform.toml`: the `state` Source, one instance per terraform
//! root (the nearest directory holding `.terraform`), answering alone, and the
//! `workspace` template over it.

use super::{age, scenario, schedule, served};
use beachcomber::provider::{FailbackConfig, InvalidationStrategy};
use serde_json::json;
use std::path::Path;

/// A terraform root at `dir`, `.terraform/environment` holding `workspace`
/// when there is one.
fn terraform_root(dir: &Path, workspace: Option<&str>) {
    std::fs::create_dir_all(dir.join(".terraform")).unwrap();
    if let Some(ws) = workspace {
        std::fs::write(dir.join(".terraform/environment"), ws).unwrap();
    }
}

/// The daemon's field is `workspace`, the selected workspace's name.
#[test]
fn terraform_workspace_field_present() {
    let s = served(&[], "");
    terraform_root(&s.project, Some("staging\n"));
    assert_eq!(
        s.get("terraform", Some(&s.project)),
        Some(json!({"workspace": "staging"}))
    );
}

/// The daemon reads the file, never `$TF_WORKSPACE`: that is the template's.
#[test]
fn terraform_no_env_read_for_tf_workspace() {
    let s = served(&[("TF_WORKSPACE", "from-env")], "");
    terraform_root(&s.project, Some("from-file"));
    assert_eq!(
        s.get("terraform.workspace", Some(&s.project)),
        Some(json!("from-file"))
    );
}

/// `.terraform` with no `environment` file is the `default` workspace.
#[test]
fn terraform_no_environment_file_defaults_to_default() {
    let s = served(&[], "");
    terraform_root(&s.project, None);
    assert_eq!(
        s.get("terraform.workspace", Some(&s.project)),
        Some(json!("default"))
    );
}

/// An `environment` file that names nothing is no workspace: absent, not "".
#[test]
fn terraform_an_empty_environment_file_names_no_workspace() {
    let s = served(&[], "");
    terraform_root(&s.project, Some(" \n"));
    assert_eq!(s.get("terraform.workspace", Some(&s.project)), None);
}

/// Without a `.terraform` there is no instance: nothing, not `default`.
#[test]
fn terraform_returns_none_without_terraform_dir() {
    let s = served(&[], "");
    assert_eq!(s.get("terraform.workspace", Some(&s.project)), None);
    assert_eq!(s.get("terraform", Some(&s.project)), None);
}

/// A directory below a root reads the root's instance; a root nested in
/// another answers alone.
#[test]
fn terraform_root_returns_module_root_from_subdir() {
    let s = served(&[], "");
    terraform_root(&s.project, Some("outer"));
    let sub = s.project.join("envs/dev");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(
        s.get("terraform.workspace", Some(&sub)),
        Some(json!("outer"))
    );
    s.ran_at("terraform", Some(&s.project), "state");

    let inner = s.project.join("modules/net");
    terraform_root(&inner, None);
    assert_eq!(
        s.get("terraform.workspace", Some(&inner)),
        Some(json!("default")),
        "the nearer root answers, with its own default"
    );
}

/// With nothing watching and nothing polling, a `terraform workspace select`
/// is seen by the next read, and a read with nothing changed runs nothing.
#[test]
fn terraform_a_workspace_selected_is_seen_by_the_next_read() {
    let s = served(&[], "");
    terraform_root(&s.project, Some("one"));
    let file = s.project.join(".terraform/environment");
    age(&file);
    assert_eq!(
        s.get("terraform.workspace", Some(&s.project)),
        Some(json!("one"))
    );
    let ran = s.ran_at("terraform", Some(&s.project), "state");
    for _ in 0..5 {
        s.get("terraform.workspace", Some(&s.project));
    }
    assert_eq!(s.ran_at("terraform", Some(&s.project), "state"), ran);

    std::fs::write(&file, "two").unwrap();
    assert_eq!(
        s.get("terraform.workspace", Some(&s.project)),
        Some(json!("two"))
    );
    std::fs::remove_file(&file).unwrap();
    assert_eq!(
        s.get("terraform.workspace", Some(&s.project)),
        Some(json!("default")),
        "the file removed"
    );
}

/// The template over the Source: the shell's `$TF_WORKSPACE` wins, and with
/// none set the workspace selected on disk answers.
#[test]
fn terraform_the_workspace_template_reads_the_shell_then_the_source() {
    let s = scenario();
    terraform_root(&s.project, Some("staging"));
    assert_eq!(
        s.get_in_env(
            "terraform.workspace",
            &s.project,
            &[("TF_WORKSPACE", "prod")]
        ),
        Some(json!("prod"))
    );
    assert_eq!(
        s.get("terraform.workspace", &s.project),
        Some(json!("staging"))
    );
}

/// The Rust provider's schedule, kept: watch `.terraform/environment`, retry
/// a failure every minute three times, the nearest root alone.
#[test]
fn terraform_keeps_its_schedule() {
    let (meta, rooted) = schedule("terraform", "state");
    assert!(rooted);
    assert_eq!(
        meta.invalidation,
        InvalidationStrategy::Watch {
            patterns: vec![".terraform/environment".into()],
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
