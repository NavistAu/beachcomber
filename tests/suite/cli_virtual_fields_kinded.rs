//! Kinded refs: `discover_refs` tells `env.*`, `comb.P.F`, `comb.P` and a
//! resolved `P.F` apart, and the resolver binds each kind its own way. The
//! discovery half is pure library; the binding half runs `comb get` against a
//! test daemon with the templates `put` to `<p>:template`.

use crate::common::daemon::TestDaemon;
use libbeachcomber::virtual_fields::Ref;
use std::path::{Path, PathBuf};

/// `src`'s refs, read in the client's engine.
fn discover_refs(src: &str) -> Vec<Ref> {
    let probe = libbeachcomber::eval::Probe::new(libbeachcomber::engine::build_engine_env(
        Default::default(),
    ));
    libbeachcomber::eval::discover_refs(&probe, src)
}

// ── Step 1.1: Ref discovery returns kinded refs ───────────────────────────────

#[test]
fn discovers_env_comb_and_resolved_refs() {
    let refs = discover_refs(
        r#"{{ env.AWS_REGION or comb.aws_profiles[env.AWS_PROFILE or "default"].region or aws.profile }}"#,
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
        refs.contains(&Ref::CombProvider("aws_profiles".into())),
        "must discover CombProvider(aws_profiles); got: {refs:?}"
    );
    assert!(
        refs.contains(&Ref::Resolved("aws".into(), "profile".into())),
        "must discover Resolved(aws, profile); got: {refs:?}"
    );
}

#[test]
fn discovers_comb_field_ref() {
    let refs = discover_refs("{{ comb.mise.python or comb.python.venv_version }}");
    assert!(
        refs.contains(&Ref::CombField("mise".into(), "python".into())),
        "must discover CombField(mise, python); got: {refs:?}"
    );
    assert!(
        refs.contains(&Ref::CombField("python".into(), "venv_version".into())),
        "must discover CombField(python, venv_version); got: {refs:?}"
    );
}

#[test]
fn discovers_comb_provider_without_field() {
    // "comb.aws_profiles" (two segments only) → CombProvider
    let refs = discover_refs("{{ comb.aws_profiles }}");
    assert!(
        refs.contains(&Ref::CombProvider("aws_profiles".into())),
        "must discover CombProvider(aws_profiles); got: {refs:?}"
    );
    // Must NOT produce a CombField entry for it
    let has_field = refs
        .iter()
        .any(|r| matches!(r, Ref::CombField(p, _) if p == "aws_profiles"));
    assert!(
        !has_field,
        "must not produce CombField for two-segment cache ref; got: {refs:?}"
    );
}

#[test]
fn a_bare_name_is_a_dynamic_ref_and_a_function_is_none() {
    // A single-segment name is used other than by attribute: the node it
    // names, read whole. Wrapped in a tag so this reaches `classify_dotted`
    // (a tagless "somename" is literal text and would find nothing
    // regardless). A function the engine registers is not a name read.
    assert_eq!(
        discover_refs("{{ somename }}"),
        vec![Ref::Dynamic("somename".into())]
    );
    assert_eq!(
        discover_refs("{{ lup('.git') }}{{ merge({}, {}) }}"),
        Vec::<Ref>::new()
    );
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

/// `comb.P` and `comb.P.F` in one expression bind the same provider: the
/// whole object stays indexable and the field read through it.
#[test]
fn comb_provider_and_comb_field_same_provider_both_visible() {
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
        r#"{"project":"{{ comb.cfgs[comb.cfgs.active].project }}","account":"{{ comb.cfgs[comb.cfgs.active].account }}"}"#,
    );

    assert_eq!(get(&d, &cwd, &[], "t.project"), "proj-default");
    assert_eq!(get(&d, &cwd, &[], "t.account"), "default@example.com");
}
