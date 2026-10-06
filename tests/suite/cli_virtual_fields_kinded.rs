//! Kinded refs: `discover_refs` tells `env.*`, `cache.P.F`, `cache.P` and a
//! resolved `P.F` apart, and the resolver binds each kind its own way. The
//! discovery half is pure library; the binding half runs `comb get` against a
//! test daemon with the templates `put` to `<p>:template`.

use crate::common::daemon::TestDaemon;
use libbeachcomber::eval::discover_refs;
use libbeachcomber::virtual_fields::Ref;
use std::path::{Path, PathBuf};

// ── Step 1.1: Ref discovery returns kinded refs ───────────────────────────────

#[test]
fn discovers_env_cache_and_resolved_refs() {
    let refs = discover_refs(
        r#"{{ env.AWS_REGION or cache.aws_profiles[env.AWS_PROFILE or "default"].region or aws.profile }}"#,
    );
    assert!(
        refs.contains(&Ref::Env("AWS_REGION".into())),
        "must discover Env(AWS_REGION); got: {refs:?}"
    );
    assert!(
        refs.contains(&Ref::Env("AWS_PROFILE".into())),
        "must discover Env(AWS_PROFILE); got: {refs:?}"
    );
    assert!(
        refs.contains(&Ref::CacheProvider("aws_profiles".into())),
        "must discover CacheProvider(aws_profiles); got: {refs:?}"
    );
    assert!(
        refs.contains(&Ref::Resolved("aws".into(), "profile".into())),
        "must discover Resolved(aws, profile); got: {refs:?}"
    );
}

#[test]
fn discovers_cache_field_ref() {
    let refs = discover_refs("{{ cache.mise.python or cache.python.venv_version }}");
    assert!(
        refs.contains(&Ref::CacheField("mise".into(), "python".into())),
        "must discover CacheField(mise, python); got: {refs:?}"
    );
    assert!(
        refs.contains(&Ref::CacheField("python".into(), "venv_version".into())),
        "must discover CacheField(python, venv_version); got: {refs:?}"
    );
}

#[test]
fn discovers_cache_provider_without_field() {
    // "cache.aws_profiles" (two segments only) → CacheProvider
    let refs = discover_refs("{{ cache.aws_profiles }}");
    assert!(
        refs.contains(&Ref::CacheProvider("aws_profiles".into())),
        "must discover CacheProvider(aws_profiles); got: {refs:?}"
    );
    // Must NOT produce a CacheField entry for it
    let has_field = refs
        .iter()
        .any(|r| matches!(r, Ref::CacheField(p, _) if p == "aws_profiles"));
    assert!(
        !has_field,
        "must not produce CacheField for two-segment cache ref; got: {refs:?}"
    );
}

#[test]
fn bare_name_is_ignored() {
    // A single-segment name with no dot is not a provider.field ref — must be
    // ignored. Wrapped in a tag so this reaches `classify_dotted` (a tagless
    // "somename" is literal text and would find nothing regardless).
    let refs = discover_refs("{{ somename }}");
    assert!(refs.is_empty(), "bare name must be ignored; got: {refs:?}");
}

#[test]
fn cwd_is_ignored() {
    // cwd is a path-expression variable, not a field ref — must be ignored.
    let refs = discover_refs("{{ cwd or env.HOME }}");
    assert!(
        !refs
            .iter()
            .any(|r| matches!(r, Ref::Resolved(p, _) if p == "cwd")),
        "cwd must be ignored; got: {refs:?}"
    );
    // env.HOME must still be discovered
    assert!(
        refs.contains(&Ref::Env("HOME".into())),
        "env.HOME must still be discovered; got: {refs:?}"
    );
}

// ── binding, through comb get ─────────────────────────────────────────────────

fn daemon() -> (TestDaemon, PathBuf) {
    let d = TestDaemon::spawn();
    let cwd = d.home.join("ws").join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    (d, cwd)
}

fn comb(d: &TestDaemon, cwd: &Path) -> assert_cmd::Command {
    let mut cmd = d.comb(cwd);
    cmd.env_remove("TF_WORKSPACE");
    cmd
}

fn put(d: &TestDaemon, cwd: &Path, key: &str, data: &str) {
    comb(d, cwd).args(["put", key, data]).assert().success();
}

fn get(d: &TestDaemon, cwd: &Path, env: &[(&str, &str)], key: &str) -> String {
    let mut cmd = comb(d, cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.args(["get", key]).assert().success();
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

/// A resolved ref to a key the daemon holds a template for renders that
/// template — `{{ terraform.workspace }}` is the managed default's cascade,
/// not a raw cache read.
#[test]
fn resolved_ref_renders_the_template_behind_the_key() {
    let (d, cwd) = daemon();
    put(&d, &cwd, "t:template.v", r#""{{ terraform.workspace }}""#);

    assert_eq!(get(&d, &cwd, &[("TF_WORKSPACE", "myws")], "t.v"), "myws");
}

/// `cache.P` and `cache.P.F` in one expression bind the same provider: the
/// whole object stays indexable and the field read through it.
#[test]
fn cache_provider_and_cache_field_same_provider_both_visible() {
    let (d, cwd) = daemon();
    put(
        &d,
        &cwd,
        "cfgs",
        r#"{"active":"default","default":{"project":"proj-default","account":"default@example.com"},"work":{"project":"proj-work","account":"work@example.com"}}"#,
    );
    put(
        &d,
        &cwd,
        "t:template",
        r#"{"project":"{{ cache.cfgs[cache.cfgs.active].project }}","account":"{{ cache.cfgs[cache.cfgs.active].account }}"}"#,
    );

    assert_eq!(get(&d, &cwd, &[], "t.project"), "proj-default");
    assert_eq!(get(&d, &cwd, &[], "t.account"), "default@example.com");
}
