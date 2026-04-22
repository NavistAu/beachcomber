use beachcomber::cli::format::render_fmt_template;
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
