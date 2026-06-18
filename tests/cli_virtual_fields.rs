//! Tests for the client-side virtual field evaluator (src/cli/virtual_fields.rs),
//! the env.* resolver, the basename filter, and related format helpers.

use beachcomber::cli::format::build_env;

// ── basename filter ──────────────────────────────────────────────────────────

#[test]
fn basename_filter_extracts_last_component() {
    let env = build_env();
    let result = env
        .render_str(r#"{{ "/home/user/.venv" | basename }}"#, ())
        .unwrap();
    assert_eq!(result, ".venv");
}

#[test]
fn basename_filter_on_plain_name() {
    let env = build_env();
    let result = env.render_str(r#"{{ "myenv" | basename }}"#, ()).unwrap();
    assert_eq!(result, "myenv");
}

#[test]
fn basename_filter_trailing_slash() {
    let env = build_env();
    let result = env
        .render_str(r#"{{ "/some/path/" | basename }}"#, ())
        .unwrap();
    // Trailing slash: last non-empty component.
    assert_eq!(result, "path");
}

#[test]
fn basename_filter_empty_string() {
    let env = build_env();
    let result = env.render_str(r#"{{ "" | basename }}"#, ()).unwrap();
    assert_eq!(result, "");
}
