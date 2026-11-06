//! `defaults/gcloud.toml`: the `gcloud_configs.config_dir` Source, one
//! instance at `~`, every gcloud configuration and the active one's name, and
//! the `gcloud` templates that index it by the shell's selector.

use super::{age, scenario, schedule, served};
use beachcomber::provider::{FailbackConfig, InvalidationStrategy};
use serde_json::json;
use std::path::Path;

/// The configuration `name` under the gcloud directory `dir`, its
/// `properties` holding `text`.
fn config(dir: &Path, name: &str, text: &str) {
    let conf = dir.join("configurations").join(format!("config_{name}"));
    std::fs::create_dir_all(&conf).unwrap();
    std::fs::write(conf.join("properties"), text).unwrap();
}

/// A configuration with a `[core]` `project` and `account`.
fn core(dir: &Path, name: &str, project: &str, account: &str) {
    config(
        dir,
        name,
        &format!("[core]\nproject = {project}\naccount = {account}\n"),
    );
}

fn active(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("active_config"), name).unwrap();
}

/// `active_config` is a string field; each configuration is a field of its
/// own, an object of `project` and `account`, with no wrapper.
#[test]
fn gcloud_configs_two_configs_and_active_config_field() {
    let s = served(&[], "");
    let dir = s.home.join(".config/gcloud");
    active(&dir, "default\n");
    core(&dir, "default", "proj-a", "a@x.com");
    core(&dir, "work", "proj-b", "b@x.com");
    assert_eq!(
        s.get("gcloud_configs", None),
        Some(json!({
            "active_config": "default",
            "default": {"project": "proj-a", "account": "a@x.com"},
            "work": {"project": "proj-b", "account": "b@x.com"},
        }))
    );
}

/// `$CLOUDSDK_CONFIG` in the daemon's environment names the directory, and
/// `active_config` names the configuration gcloud would use.
#[test]
fn gcloud_reads_active_config_indirection() {
    let elsewhere = tempfile::tempdir().unwrap();
    let dir = elsewhere.path();
    active(dir, "staging\n");
    core(dir, "staging", "my-project", "user@example.com");
    let s = served(&[("CLOUDSDK_CONFIG", dir.to_str().unwrap())], "");
    core(&s.home.join(".config/gcloud"), "default", "p", "a");
    assert_eq!(
        s.get("gcloud_configs", None),
        Some(json!({
            "active_config": "staging",
            "staging": {"project": "my-project", "account": "user@example.com"},
        }))
    );
}

/// `$CLOUDSDK_ACTIVE_CONFIG_NAME` in the daemon's environment changes
/// nothing: the file names the active configuration, the shell's selector is
/// the template's.
#[test]
fn gcloud_active_config_name_env_does_not_override_file() {
    let s = served(&[("CLOUDSDK_ACTIVE_CONFIG_NAME", "work")], "");
    let dir = s.home.join(".config/gcloud");
    active(&dir, "default");
    core(&dir, "default", "proj-a", "a@x.com");
    assert_eq!(
        s.get("gcloud_configs.active_config", None),
        Some(json!("default"))
    );
}

/// No `active_config` file: no `active_config`, and the configurations all
/// the same.
#[test]
fn gcloud_configs_without_active_config_file_still_enumerates() {
    let s = served(&[], "");
    core(
        &s.home.join(".config/gcloud"),
        "default",
        "proj-a",
        "a@x.com",
    );
    assert_eq!(
        s.get("gcloud_configs", None),
        Some(json!({"default": {"project": "proj-a", "account": "a@x.com"}}))
    );
}

/// An empty directory, or none at all, is nothing.
#[test]
fn gcloud_configs_empty_dir_returns_empty() {
    let s = served(&[], "");
    assert_eq!(s.get("gcloud_configs", None), None);
    std::fs::create_dir_all(s.home.join(".config/gcloud/configurations")).unwrap();
    assert_eq!(s.get("gcloud_configs", None), None);
}

/// A configuration with neither a project nor an account is left out, and a
/// `[core]` with only one keeps that one.
#[test]
fn gcloud_configs_skips_config_with_empty_project_and_account() {
    let s = served(&[], "");
    let dir = s.home.join(".config/gcloud");
    active(&dir, "default");
    config(&dir, "empty", "[core]\n");
    config(&dir, "blank", "[core]\nproject =\naccount =\n");
    config(&dir, "elsewhere", "[compute]\nzone = x\n");
    config(&dir, "half", "[core]\naccount = h@x.com\n");
    core(&dir, "default", "proj-a", "a@x.com");
    assert_eq!(
        s.get("gcloud_configs", None),
        Some(json!({
            "active_config": "default",
            "default": {"project": "proj-a", "account": "a@x.com"},
            "half": {"account": "h@x.com"},
        }))
    );
}

/// `projectid = …` is not `project`.
#[test]
fn gcloud_project_strip_not_greedy() {
    let s = served(&[], "");
    let dir = s.home.join(".config/gcloud");
    config(
        &dir,
        "default",
        "[core]\nprojectid = should-not-match\nproject = correct-project\n",
    );
    assert_eq!(
        s.get("gcloud_configs.default.project", None),
        Some(json!("correct-project"))
    );
}

/// A `[ core ]` header padded inside its brackets is the core section.
#[test]
fn gcloud_padded_core_section_header_is_parsed() {
    let s = served(&[], "");
    let dir = s.home.join(".config/gcloud");
    config(
        &dir,
        "default",
        "[ core ]\nproject = padded-project\naccount = padded@x.com\n",
    );
    assert_eq!(
        s.get("gcloud_configs.default", None),
        Some(json!({"project": "padded-project", "account": "padded@x.com"}))
    );
}

/// An empty `$CLOUDSDK_CONFIG` is unset, as gcloud reads it: the default
/// directory. (The Rust provider read a relative `active_config`.)
#[test]
fn gcloud_an_empty_cloudsdk_config_is_the_default() {
    let s = served(&[("CLOUDSDK_CONFIG", "")], "");
    let dir = s.home.join(".config/gcloud");
    active(&dir, "default");
    assert_eq!(
        s.get("gcloud_configs.active_config", None),
        Some(json!("default"))
    );
}

/// With nothing watching and nothing polling, `gcloud config configurations
/// activate` (the `active_config` file rewritten) and a new configuration are
/// each seen by the next read, where the Rust provider, which only polled,
/// answered the old record for up to a minute; a read with nothing changed
/// runs nothing.
#[test]
fn gcloud_an_activation_and_a_new_configuration_are_seen_by_the_next_read() {
    let s = served(&[], "");
    let dir = s.home.join(".config/gcloud");
    active(&dir, "default");
    core(&dir, "default", "proj-a", "a@x.com");
    for path in [
        dir.join("active_config"),
        dir.join("configurations/config_default/properties"),
        dir.join("configurations/config_default"),
        dir.join("configurations"),
    ] {
        age(&path);
    }
    assert_eq!(
        s.get("gcloud_configs.active_config", None),
        Some(json!("default"))
    );
    let ran = s.ran_at("gcloud_configs", None, "config_dir");
    for _ in 0..5 {
        s.get("gcloud_configs.active_config", None);
    }
    assert_eq!(s.ran_at("gcloud_configs", None, "config_dir"), ran);

    active(&dir, "work");
    assert_eq!(
        s.get("gcloud_configs.active_config", None),
        Some(json!("work")),
        "activate"
    );
    core(&dir, "work", "proj-b", "b@x.com");
    assert_eq!(
        s.get("gcloud_configs.work.project", None),
        Some(json!("proj-b")),
        "create"
    );
}

/// The `gcloud` templates index the Source by the shell's selector: with
/// none, the active configuration's project and account; with
/// `$CLOUDSDK_ACTIVE_CONFIG_NAME`, that one's; `$CLOUDSDK_CORE_PROJECT` wins.
#[test]
fn gcloud_the_templates_render_over_the_source() {
    // SAFETY: nextest gives each test its own process; the daemon starts below.
    unsafe { std::env::remove_var("CLOUDSDK_CONFIG") };
    let s = scenario();
    let dir = s.daemon.home.join(".config/gcloud");
    active(&dir, "default\n");
    core(&dir, "default", "proj-default", "default@example.com");
    core(&dir, "work", "proj-work", "work@example.com");
    assert_eq!(
        s.get("gcloud", &s.project),
        Some(json!({"project": "proj-default", "account": "default@example.com"}))
    );
    assert_eq!(
        s.get_in_env(
            "gcloud.account",
            &s.project,
            &[("CLOUDSDK_ACTIVE_CONFIG_NAME", "work")]
        ),
        Some(json!("work@example.com"))
    );
    assert_eq!(
        s.get_in_env(
            "gcloud.project",
            &s.project,
            &[("CLOUDSDK_CORE_PROJECT", "explicit")]
        ),
        Some(json!("explicit"))
    );
}

/// The Rust provider's schedule and shape, kept: one Source, `config_dir`,
/// with no root, polled every minute twice, a failure retried every 30 s
/// three times; its fields are `active_config` and the configurations.
#[test]
fn gcloud_keeps_its_schedule() {
    let (meta, rooted) = schedule("gcloud_configs", "config_dir");
    assert!(!rooted);
    assert!(
        meta.fields.iter().all(|f| f.name.starts_with('<')),
        "a whole node: {:?}",
        meta.fields
    );
    assert_eq!(
        meta.invalidation,
        InvalidationStrategy::Poll {
            interval_secs: 60,
            count: 2,
        }
    );
    assert_eq!(
        meta.failback,
        FailbackConfig {
            reattempts: 3,
            interval_secs: 30
        }
    );
    assert!(!meta.nearest);
}
