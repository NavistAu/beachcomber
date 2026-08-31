//! Tests for the CLI's `-f fmt` rendering and for the value-expression
//! rendering it now shares with `comb eval` and config virtual fields.
//!
//! Task 3 deleted `src/cli/format.rs`'s hand-rolled `{{ }}` scanner
//! (`find_eval_template_pairs` / `find_eval_template_refs`) and its
//! `render_eval_template`: reference discovery is `eval::discover_refs` and
//! rendering is `eval::render_template`, both in `libbeachcomber`. The scanner
//! cases that had no library equivalent are kept below, restated against
//! `discover_refs`.

use beachcomber::cli::format::render_fmt_template;
use beachcomber::provider::{ProviderResult, Value};
use libbeachcomber::eval::{discover_refs, render_template};
use libbeachcomber::virtual_fields::Ref;

fn resolved(provider: &str, field: &str) -> Ref {
    Ref::Resolved(provider.into(), field.into())
}

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
    let out = render_fmt_template("{% if dirty %}*{% endif %}{{ branch }}", &r).unwrap();
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
    let out =
        render_template("hostname: {{ hostname.value }}, load: {{ load.one }}", &ctx).unwrap();
    assert_eq!(out, "hostname: me-laptop, load: 0.42");
}

#[test]
fn eval_template_truncate_filter_works_on_fields() {
    let ctx = serde_json::json!({
        "git": {"sha": "abcdef1234567890"}
    });
    let out = render_template("{{ git.sha | truncate(7) }}", &ctx).unwrap();
    assert_eq!(out, "abcdef1...");
}

#[test]
fn eval_template_conditional_works() {
    let ctx = serde_json::json!({
        "git": {"dirty": true, "branch": "main"}
    });
    let out = render_template("{% if git.dirty %}*{% endif %}{{ git.branch }}", &ctx).unwrap();
    assert_eq!(out, "*main");
}

#[test]
fn eval_template_default_filter_for_missing_field() {
    let ctx = serde_json::json!({"git": {"branch": "main"}});
    let out = render_template("{{ git.tag | default('no-tag') }}", &ctx).unwrap();
    assert_eq!(out, "no-tag");
}

#[test]
fn eval_template_single_brace_is_literal() {
    let ctx = serde_json::json!({"git": {"branch": "main"}});
    let out = render_template("{git.branch}", &ctx).unwrap();
    assert_eq!(out, "{git.branch}");
}

// --- reference discovery (was the deleted `find_eval_template_pairs`) ---

#[test]
fn discover_refs_finds_refs_in_for_block() {
    assert_eq!(
        discover_refs("{% for x in items.list %}{{ x }}{% endfor %}"),
        vec![resolved("items", "list")]
    );
}

#[test]
fn discover_refs_skips_comment_blocks() {
    assert_eq!(
        discover_refs("{# git.dirty #}{{ hostname.name }}"),
        vec![resolved("hostname", "name")]
    );
}

#[test]
fn discover_refs_handles_whitespace_control_dashes() {
    assert_eq!(
        discover_refs("{%- if git.dirty -%}*{%- endif -%}"),
        vec![resolved("git", "dirty")]
    );
}

#[test]
fn discover_refs_combines_block_and_expression_refs() {
    assert_eq!(
        discover_refs(
            "{% if git.dirty %}dirty:{{ git.branch }}{% else %}{{ git.branch }}{% endif %}"
        ),
        vec![resolved("git", "branch"), resolved("git", "dirty")]
    );
}

#[test]
fn discover_refs_skips_dotted_string_literals() {
    // A dotted string literal is not a ref.
    assert_eq!(
        discover_refs(r#"{{ git.branch or "foo.bar" }}"#),
        vec![resolved("git", "branch")]
    );
}

#[test]
fn eval_template_accepts_tab_and_newline_whitespace_inside_braces() {
    let ctx = serde_json::json!({"git": {"branch": "main"}});
    let out = render_template("{{\tgit.branch }}", &ctx).unwrap();
    assert_eq!(out, "main");

    assert_eq!(
        discover_refs("{{\tgit.branch }}"),
        vec![resolved("git", "branch")]
    );
}
