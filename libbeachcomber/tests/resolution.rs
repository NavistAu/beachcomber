//! Path expressions evaluate through libbeachcomber's PUBLIC API only — no
//! daemon, no ambient `std::env`, no ambient cwd.
//!
//! Every `env`/`cwd` input below is data the test constructs itself and hands
//! to the API. Reading ambient process state here would defeat the point of
//! this suite: the property under protection is that evaluation never reaches
//! outside the values it's given.

use libbeachcomber::path_expr::{evaluate_path, path_expression_for};
use std::collections::HashMap;

/// Case 3: a path expression evaluated over a supplied `cwd` selects the
/// expected cache coordinate.
#[test]
fn path_expression_over_cwd_selects_cache_coordinate() {
    let mut overrides: HashMap<String, String> = HashMap::new();
    overrides.insert(
        "myproject".to_string(),
        "{{ 'workspace-a' if cwd == '/Users/x/repo-a' else 'workspace-b' }}".to_string(),
    );

    let expr = path_expression_for("myproject", &overrides).expect("path expression declared");

    let env_vars: HashMap<String, String> = HashMap::new();

    let coord_a = evaluate_path(&expr, "/Users/x/repo-a", &env_vars);
    assert_eq!(coord_a, Some("workspace-a".to_string()));

    let coord_b = evaluate_path(&expr, "/Users/x/repo-b", &env_vars);
    assert_eq!(coord_b, Some("workspace-b".to_string()));
}

/// Case 3a: a path expression written as a single `{{ }}` tag compiles and
/// resolves — canon `field_resolution.md` §"`env.*` namespace" says `env.*`
/// is available "in path expressions (`{{ env.X }}`)".
#[test]
fn path_expression_single_tag_form_resolves() {
    let mut env_vars: HashMap<String, String> = HashMap::new();
    env_vars.insert("HOME".to_string(), "/home/tester".to_string());

    let tagged = evaluate_path(
        "{{ env.KUBECONFIG or '~/.kube/config' }}",
        "/Users/x/repo-a",
        &env_vars,
    );
    assert_eq!(tagged, Some("/home/tester/.kube/config".to_string()));

    // The env term wins too — not just the literal fallback.
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
