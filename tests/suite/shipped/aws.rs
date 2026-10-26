//! `defaults/aws.toml`: the `aws_profiles.config_file` Source, one instance
//! at `~`, every profile in the AWS config file, and the `aws` templates that
//! index it by the shell's selector.

use super::{age, scenario, schedule, served};
use beachcomber::provider::{FailbackConfig, InvalidationStrategy};
use serde_json::json;
use std::path::Path;

/// `~/.aws/config` under `home`, holding `text`.
fn aws_config(home: &Path, text: &str) -> std::path::PathBuf {
    let file = home.join(".aws/config");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, text).unwrap();
    file
}

/// Each profile is a field of its own, an object holding its `region`, with
/// no wrapper and no other key.
#[test]
fn aws_profiles_default_and_staging_profiles() {
    let s = served(&[], "");
    aws_config(
        &s.home,
        "[default]\nregion = eu-west-1\noutput = json\n\n[profile staging]\nregion = us-east-1\n",
    );
    assert_eq!(
        s.get("aws_profiles", None),
        Some(json!({"default": {"region": "eu-west-1"}, "staging": {"region": "us-east-1"}}))
    );
    assert_eq!(
        s.get("aws_profiles.staging.region", Some(&s.project)),
        Some(json!("us-east-1")),
        "read from any directory: the one instance at ~"
    );
}

/// The daemon publishes every profile, blind to the `$AWS_PROFILE` its own
/// environment holds: selecting one is the template's.
#[test]
fn aws_profiles_published_regardless_of_aws_profile_env() {
    let s = served(&[("AWS_PROFILE", "staging")], "");
    aws_config(
        &s.home,
        "[default]\nregion = eu-west-1\n\n[profile staging]\nregion = us-east-1\n",
    );
    let profiles = s.get("aws_profiles", None).unwrap();
    assert!(profiles.get("default").is_some(), "{profiles}");
    assert!(profiles.get("staging").is_some(), "{profiles}");
}

/// No config file is nothing at all, not an error.
#[test]
fn aws_profiles_missing_config_returns_empty() {
    let s = served(&[], "");
    assert_eq!(s.get("aws_profiles", None), None);
}

/// A profile with no region, or an empty one, is left out.
#[test]
fn aws_profiles_skips_profiles_with_no_region() {
    let s = served(&[], "");
    aws_config(
        &s.home,
        "[default]\noutput = json\n\n[profile staging]\nregion = us-east-1\n\n[profile blank]\nregion =\n",
    );
    assert_eq!(
        s.get("aws_profiles", None),
        Some(json!({"staging": {"region": "us-east-1"}}))
    );
}

/// Other sections (`[sso-session …]`) between profiles name no profile and
/// leak no key.
#[test]
fn aws_profiles_unknown_section_does_not_leak() {
    let s = served(&[], "");
    aws_config(
        &s.home,
        "[default]\nregion = eu-west-1\n\n[sso-session prod]\nstart_url = https://x\nregion = ap-south-1\n\n[profile staging]\nregion = us-east-1\n",
    );
    assert_eq!(
        s.get("aws_profiles", None),
        Some(json!({"default": {"region": "eu-west-1"}, "staging": {"region": "us-east-1"}}))
    );
}

/// A section header padded inside its brackets names its profile all the
/// same, and so does a padded profile name.
#[test]
fn aws_profiles_interior_spaces_in_section_header_recognised() {
    let s = served(&[], "");
    aws_config(
        &s.home,
        "[ default ]\nregion = ap-southeast-2\n[profile   ops ]\nregion = us-west-2\n",
    );
    assert_eq!(
        s.get("aws_profiles", None),
        Some(json!({"default": {"region": "ap-southeast-2"}, "ops": {"region": "us-west-2"}}))
    );
}

/// `$AWS_CONFIG_FILE` in the daemon's environment names the file read, and
/// with nothing watching and nothing polling a rewrite of it is seen by the
/// next read: the watch names only the default path, the read checks the
/// file the line read.
#[test]
fn aws_profiles_aws_config_file_is_read_and_checked() {
    let elsewhere = tempfile::tempdir().unwrap();
    let file = elsewhere.path().join("aws-config");
    std::fs::write(&file, "[profile ci]\nregion = eu-north-1\n").unwrap();
    age(&file);
    let s = served(&[("AWS_CONFIG_FILE", file.to_str().unwrap())], "");
    // The default path, the watch declaration's, is an input too.
    age(&aws_config(&s.home, "[default]\nregion = us-east-1\n"));
    age(&s.home.join(".aws"));
    assert_eq!(
        s.get("aws_profiles", None),
        Some(json!({"ci": {"region": "eu-north-1"}})),
        "the default path is not read"
    );
    let ran = s.ran_at("aws_profiles", None, "config_file");
    for _ in 0..5 {
        s.get("aws_profiles", None);
    }
    assert_eq!(s.ran_at("aws_profiles", None, "config_file"), ran);
    std::fs::write(&file, "[profile ci]\nregion = eu-west-3\n").unwrap();
    assert_eq!(
        s.get("aws_profiles.ci.region", None),
        Some(json!("eu-west-3"))
    );
}

/// An empty `$AWS_CONFIG_FILE` is unset: the default file is read.
#[test]
fn aws_profiles_an_empty_aws_config_file_is_the_default() {
    let s = served(&[("AWS_CONFIG_FILE", "")], "");
    aws_config(&s.home, "[default]\nregion = us-east-1\n");
    assert_eq!(
        s.get("aws_profiles.default.region", None),
        Some(json!("us-east-1"))
    );
}

/// The `aws` templates index the Source by the shell's selector: with none,
/// `aws.region` is the default profile's; with `$AWS_PROFILE`, that profile's.
#[test]
fn aws_the_templates_render_over_the_source() {
    // SAFETY: nextest gives each test its own process; the daemon starts below.
    unsafe { std::env::remove_var("AWS_CONFIG_FILE") };
    let s = scenario();
    aws_config(
        &s.daemon.home,
        "[default]\nregion = us-east-1\n\n[profile staging]\nregion = eu-west-1\n",
    );
    assert_eq!(s.get("aws.region", &s.project), Some(json!("us-east-1")));
    assert_eq!(
        s.get_in_env("aws.region", &s.project, &[("AWS_PROFILE", "staging")]),
        Some(json!("eu-west-1"))
    );
}

/// The Rust provider's schedule and shape, kept but for the deliberate
/// difference: one Source, `config_file`, with no root, whose fields are the
/// profiles; it watches `~/.aws/config` (the default path even when
/// `$AWS_CONFIG_FILE` names another: a watch is a constant), polls every
/// minute twice, and retries a failure every 30 s three times.
#[test]
fn aws_keeps_its_schedule_and_watches_the_default_path() {
    let home = std::env::var("HOME").unwrap();
    // SAFETY: nextest gives each test its own process, and nothing else
    // reads the environment here.
    unsafe { std::env::set_var("AWS_CONFIG_FILE", "/elsewhere/aws/config") };
    let (meta, rooted) = schedule("aws_profiles", "config_file");
    assert!(!rooted);
    let registry = crate::common::registry::with_shipped_defaults();
    let sources: Vec<String> = registry
        .provider_sources("aws_profiles")
        .unwrap()
        .iter()
        .map(|s| s.name.clone())
        .collect();
    assert_eq!(sources, ["config_file"], "no profile Source");
    assert!(
        meta.fields.iter().all(|f| f.name.starts_with('<')),
        "the fields are the profiles: {:?}",
        meta.fields
    );
    assert_eq!(
        meta.invalidation,
        InvalidationStrategy::WatchAndPoll {
            patterns: vec![],
            abs_paths: vec![format!("{home}/.aws/config")],
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

/// The file is INI as `fromini` reads it: a quoted value is unquoted and `:`
/// separates a key as `=` does (as the AWS CLI reads them; the Rust parser
/// kept the quotes and passed over a `:` line).
#[test]
fn aws_profiles_quoted_values_and_colon_separators() {
    let s = served(&[], "");
    aws_config(
        &s.home,
        "[default]\nregion = \"us-east-1\"\n[profile ops]\nregion: us-west-2\n",
    );
    assert_eq!(
        s.get("aws_profiles", None),
        Some(json!({"default": {"region": "us-east-1"}, "ops": {"region": "us-west-2"}}))
    );
}

/// A file `fromini` cannot parse (an unterminated quote) fails the Source:
/// the read is a miss, with nothing half-read, and the next good file is
/// read again.
#[test]
fn aws_profiles_a_file_that_does_not_parse_fails_the_source() {
    let s = served(&[], "");
    let file = aws_config(&s.home, "[default]\nregion = 'unterminated\n");
    let answer = s.ask(&json!({"op": "get", "key": "aws_profiles", "wait": "forever"}));
    assert_eq!(answer["ok"], json!(true), "{answer}");
    assert!(answer.get("data").is_none(), "{answer}");
    std::fs::write(&file, "[default]\nregion = us-east-1\n").unwrap();
    assert_eq!(
        s.get("aws_profiles.default.region", None),
        Some(json!("us-east-1"))
    );
}
