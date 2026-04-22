use beachcomber::cli::format::{
    find_eval_template_pairs, find_eval_template_refs, render_eval_template, render_fmt_template,
};
use beachcomber::provider::{ProviderResult, Value};

fn result_with_fields(fields: &[(&str, Value)]) -> ProviderResult {
    let mut r = ProviderResult::new();
    for (k, v) in fields {
        r.insert(k.to_string(), v.clone());
    }
    r
}

#[test]
fn fmt_template_renders_double_brace_variables() {
    let r = result_with_fields(&[
        ("branch", Value::String("main".into())),
        ("dirty", Value::Bool(false)),
    ]);
    let out = render_fmt_template("{{ branch }} ({{ dirty }})", &r).unwrap();
    assert_eq!(out, "main (false)");
}

#[test]
fn fmt_template_truncate_filter_works() {
    let r = result_with_fields(&[("sha", Value::String("abcdef1234567890".into()))]);
    let out = render_fmt_template("{{ sha | truncate(7) }}", &r).unwrap();
    assert_eq!(out, "abcdef1...");
}

#[test]
fn fmt_template_default_filter_works() {
    let r = result_with_fields(&[("branch", Value::String("main".into()))]);
    let out = render_fmt_template("{{ missing | default('?') }}", &r).unwrap();
    assert_eq!(out, "?");
}

#[test]
fn fmt_template_conditional_works() {
    let r = result_with_fields(&[
        ("branch", Value::String("main".into())),
        ("dirty", Value::Bool(true)),
    ]);
    let out = render_fmt_template(
        "{% if dirty %}*{% endif %}{{ branch }}",
        &r,
    )
    .unwrap();
    assert_eq!(out, "*main");
}

#[test]
fn fmt_template_single_brace_is_literal() {
    // Clean break: single-brace is no longer substitution; it's literal.
    let r = result_with_fields(&[("branch", Value::String("main".into()))]);
    let out = render_fmt_template("{branch}", &r).unwrap();
    assert_eq!(out, "{branch}");
}

// --- eval template tests ---

#[test]
fn eval_template_renders_provider_field_refs() {
    let ctx = serde_json::json!({
        "hostname": {"value": "me-laptop"},
        "load": {"one": 0.42, "five": 0.3, "fifteen": 0.25}
    });
    let out = render_eval_template(
        "hostname: {{ hostname.value }}, load: {{ load.one }}",
        &ctx,
    )
    .unwrap();
    assert_eq!(out, "hostname: me-laptop, load: 0.42");
}

#[test]
fn eval_template_truncate_filter_works_on_fields() {
    let ctx = serde_json::json!({
        "git": {"sha": "abcdef1234567890"}
    });
    let out = render_eval_template("{{ git.sha | truncate(7) }}", &ctx).unwrap();
    assert_eq!(out, "abcdef1...");
}

#[test]
fn eval_template_conditional_works() {
    let ctx = serde_json::json!({
        "git": {"dirty": true, "branch": "main"}
    });
    let out = render_eval_template(
        "{% if git.dirty %}*{% endif %}{{ git.branch }}",
        &ctx,
    )
    .unwrap();
    assert_eq!(out, "*main");
}

#[test]
fn eval_template_default_filter_for_missing_field() {
    let ctx = serde_json::json!({"git": {"branch": "main"}});
    let out = render_eval_template("{{ git.tag | default('no-tag') }}", &ctx).unwrap();
    assert_eq!(out, "no-tag");
}

#[test]
fn eval_template_single_brace_is_literal() {
    let ctx = serde_json::json!({"git": {"branch": "main"}});
    let out = render_eval_template("{git.branch}", &ctx).unwrap();
    assert_eq!(out, "{git.branch}");
}

#[test]
fn find_eval_template_refs_extracts_provider_names() {
    let refs = find_eval_template_refs(
        "{{ git.branch }} {{ hostname.value }} {{ git.sha | truncate(7) }}",
    );
    assert!(refs.contains("git"));
    assert!(refs.contains("hostname"));
}

#[test]
fn find_eval_template_pairs_extracts_provider_field_pairs() {
    let pairs = find_eval_template_pairs("{{ git.branch }} {{ hostname.value }} {{ git.sha }}");
    assert!(pairs.contains(&("git".to_string(), "branch".to_string())));
    assert!(pairs.contains(&("hostname".to_string(), "value".to_string())));
    assert!(pairs.contains(&("git".to_string(), "sha".to_string())));
}

#[test]
fn find_eval_template_pairs_ignores_bare_vars() {
    // {{ name }} with no dot should not produce a pair
    let pairs = find_eval_template_pairs("{{ name }}");
    assert!(pairs.is_empty());
}

#[test]
fn find_eval_template_refs_deduplicates_providers() {
    let refs = find_eval_template_refs("{{ git.branch }} {{ git.sha }}");
    assert_eq!(refs.len(), 1);
    assert!(refs.contains("git"));
}

#[test]
fn eval_template_accepts_tab_and_newline_whitespace_inside_braces() {
    let ctx = serde_json::json!({"git": {"branch": "main"}});
    // Tab after {{ — scanner must still detect git.branch
    let out = render_eval_template(
        "{{\tgit.branch }}",
        &ctx,
    )
    .unwrap();
    assert_eq!(out, "main");

    // Also verify the pair extraction sees the ref
    let pairs = find_eval_template_pairs("{{\tgit.branch }}");
    assert!(pairs.iter().any(|(p, f)| p == "git" && f == "branch"));
}
