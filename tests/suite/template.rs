//! Tests for the CLI's `-f fmt` rendering and for the value-expression
//! rendering it now shares with `comb eval` and the `:template` facet.
//!
//! Task 3 deleted `src/cli/format.rs`'s hand-rolled `{{ }}` scanner
//! (`find_eval_template_pairs` / `find_eval_template_refs`) and its
//! `render_eval_template`: reference discovery is `eval::discover_refs` and
//! rendering is `eval::render_template`, both in `libbeachcomber`. The scanner
//! cases that had no library equivalent are kept below, restated against
//! `discover_refs`.
//!
//! Plan 4 Task 1 deleted `render_fmt_template`, the `ProviderResult`-keyed
//! wrapper around `render_fmt_template_json` that nothing in `src/` called
//! any more; its `fmt_template_*` cases went with it, each already restated
//! below as an `eval_template_*` case exercising the same `render_template`.

use libbeachcomber::eval::{discover_refs, render_template};
use libbeachcomber::virtual_fields::Ref;

fn resolved(provider: &str, field: &str) -> Ref {
    Ref::Resolved(provider.into(), field.into())
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

#[test]
fn render_template_fg_filter_wraps_in_truecolor_sgr() {
    let ctx = serde_json::json!({"git": {"branch": "main"}});
    let out = render_template("{{ git.branch | fg('#00ff00') }}", &ctx).unwrap();
    assert_eq!(out, "\x1b[38;2;0;255;0mmain\x1b[0m");
}
