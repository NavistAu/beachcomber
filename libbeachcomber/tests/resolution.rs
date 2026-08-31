//! Task 1.6: prove in-process resolution works through libbeachcomber's
//! PUBLIC API only — no daemon, no ambient `std::env`, no ambient cwd.
//!
//! Every `env`/`cwd` input below is data the test constructs itself and hands
//! to the API. Reading ambient process state here would defeat the point of
//! this suite: the property under protection is that resolution never reaches
//! outside the values it's given.

use libbeachcomber::eval;
use libbeachcomber::path_expr::{evaluate_path, path_expression_for};
use libbeachcomber::virtual_fields::{EvalContext, VirtualFields};
use serde_json::json;
use std::collections::{HashMap, HashSet};

/// Case 1: a virtual field whose expression references a `cache.*` value
/// resolves to that value.
#[test]
fn virtual_field_resolves_cache_reference() {
    let vf = VirtualFields::with_config_overrides([(
        ("myprov".to_string(), "myfield".to_string()),
        "cache.otherprov.otherfield".to_string(),
    )]);

    let env_vars: HashMap<String, String> = HashMap::new();
    let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    daemon_data.insert("otherprov.otherfield".to_string(), json!("hello-value"));

    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };

    let result = vf
        .evaluate("myprov", "myfield", &ctx, &mut HashSet::new())
        .expect("evaluate");

    assert_eq!(result, json!("hello-value"));
}

/// Case 2: a cascade `env.X or cache.p.f` takes the env term when present,
/// falls through to the cache term when env is absent, and a total miss
/// (neither term available) yields `""` rather than an error.
#[test]
fn cascade_env_or_cache_falls_through_to_empty_string_on_miss() {
    // terraform.workspace is a built-in: "env.TF_WORKSPACE or cache.terraform.workspace"
    let vf = VirtualFields::defaults_only();

    // (a) env present, cache also present -> env term wins.
    let mut env_vars: HashMap<String, String> = HashMap::new();
    env_vars.insert("TF_WORKSPACE".to_string(), "prod-env".to_string());
    let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    daemon_data.insert("terraform.workspace".to_string(), json!("cache-env"));
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate("terraform", "workspace", &ctx, &mut HashSet::new())
        .expect("evaluate");
    assert_eq!(result, json!("prod-env"));

    // (b) env absent, cache present -> falls through to the cache term.
    let empty_env: HashMap<String, String> = HashMap::new();
    let ctx = EvalContext {
        env_vars: &empty_env,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate("terraform", "workspace", &ctx, &mut HashSet::new())
        .expect("evaluate");
    assert_eq!(result, json!("cache-env"));

    // (c) both absent -> a miss yields "" (empty string), not an error.
    let empty_daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    let ctx = EvalContext {
        env_vars: &empty_env,
        daemon_data: &empty_daemon_data,
    };
    let result = vf
        .evaluate("terraform", "workspace", &ctx, &mut HashSet::new())
        .expect("evaluate");
    assert_eq!(result, json!(""));
}

/// Case 3: a path expression evaluated over a supplied `cwd` selects the
/// expected cache coordinate.
#[test]
fn path_expression_over_cwd_selects_cache_coordinate() {
    let mut overrides: HashMap<String, String> = HashMap::new();
    overrides.insert(
        "myproject".to_string(),
        "'workspace-a' if cwd == '/Users/x/repo-a' else 'workspace-b'".to_string(),
    );

    let expr = path_expression_for("myproject", &overrides).expect("path expression declared");

    let env_vars: HashMap<String, String> = HashMap::new();

    let coord_a = evaluate_path(&expr, "/Users/x/repo-a", &env_vars);
    assert_eq!(coord_a, Some("workspace-a".to_string()));

    let coord_b = evaluate_path(&expr, "/Users/x/repo-b", &env_vars);
    assert_eq!(coord_b, Some("workspace-b".to_string()));
}

/// Case 3a: a path expression written as a single `{{ }}` tag is the same
/// expression as the bare form — canon `field_resolution.md` §"`env.*`
/// namespace" says `env.*` is available "in path expressions (`{{ env.X }}`)",
/// so the tagged form has to compile rather than fail and collapse the
/// provider to the global slot.
#[test]
fn path_expression_single_tag_equals_bare_form() {
    let mut env_vars: HashMap<String, String> = HashMap::new();
    env_vars.insert("HOME".to_string(), "/home/tester".to_string());

    let bare = evaluate_path(
        "env.KUBECONFIG or '~/.kube/config'",
        "/Users/x/repo-a",
        &env_vars,
    );
    let tagged = evaluate_path(
        "{{ env.KUBECONFIG or '~/.kube/config' }}",
        "/Users/x/repo-a",
        &env_vars,
    );
    assert_eq!(bare, Some("/home/tester/.kube/config".to_string()));
    assert_eq!(tagged, bare);

    // The env term wins in both forms too — not just the literal fallback.
    env_vars.insert("KUBECONFIG".to_string(), "/etc/kube.yaml".to_string());
    assert_eq!(
        evaluate_path(
            "{{ env.KUBECONFIG or '~/.kube/config' }}",
            "/Users/x/repo-a",
            &env_vars
        ),
        Some("/etc/kube.yaml".to_string())
    );
}

/// Case 3b: a path expression in the template form renders to its string.
/// Nothing about a path needs the single-tag form's type preservation — the
/// result is a path either way — so literal text around a tag is legal and
/// yields the rendered path.
#[test]
fn path_expression_template_form_yields_rendered_string() {
    let mut env_vars: HashMap<String, String> = HashMap::new();
    env_vars.insert("SELECTOR".to_string(), "staging".to_string());

    assert_eq!(
        evaluate_path("/srv/{{ env.SELECTOR }}/config", "/Users/x", &env_vars),
        Some("/srv/staging/config".to_string())
    );

    // A template that renders to nothing is empty/falsy — the global slot.
    assert_eq!(
        evaluate_path(
            "{{ env.MISSING }}{{ env.ALSO_MISSING }}",
            "/Users/x",
            &env_vars
        ),
        None
    );
}

/// Case 4: an expression using the `truncate` and `basename` filters
/// resolves — proving the filter semantics moved with Task 1.2, not just
/// the symbols.
#[test]
fn truncate_and_basename_filters_resolve() {
    let vf = VirtualFields::defaults_only();

    let mut env_vars: HashMap<String, String> = HashMap::new();
    env_vars.insert("LONGVAR".to_string(), "abcdefghij".to_string());
    env_vars.insert("PYVAR".to_string(), "/foo/bar/baz".to_string());
    let daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };

    let truncated = eval::evaluate("env.LONGVAR | truncate(5)", &vf, &ctx).expect("truncate");
    assert_eq!(truncated, json!("abcde..."));

    let based = eval::evaluate("env.PYVAR | basename", &vf, &ctx).expect("basename");
    assert_eq!(based, json!("baz"));
}
