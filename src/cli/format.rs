use minijinja::Environment;
use serde_json;
use std::collections::HashSet;

use crate::provider::{ProviderResult, Value as ProvValue};

/// Build a minijinja `Environment` with the shared custom filters pre-registered.
///
/// Shared by all rendering helpers so the `truncate` filter (and any future
/// additions) are available in `fmt`, `eval`, and any other template surface.
pub(crate) fn build_env<'a>() -> Environment<'a> {
    let mut env = Environment::new();
    env.add_filter("truncate", truncate_filter);
    env
}

/// Render a minijinja template against a `ProviderResult`.
///
/// Templates use `{{ field }}` double-brace syntax. Filters available:
/// - `truncate(N)` — first N chars + "..." if longer (custom; not in minijinja builtins)
/// - All standard minijinja `builtins` filters: `default`, `upper`, `lower`, `trim`, etc.
/// - Conditionals: `{% if dirty %}*{% endif %}`
///
/// Single-brace `{field}` is treated as a literal string (no substitution).
pub fn render_fmt_template(template: &str, result: &ProviderResult) -> Result<String, String> {
    let env = build_env();

    let ctx: serde_json::Map<String, serde_json::Value> = result
        .fields
        .iter()
        .map(|(k, v)| (k.clone(), prov_value_to_json(v)))
        .collect();

    env.render_str(template, serde_json::Value::Object(ctx))
        .map_err(|e| e.to_string())
}

/// Render a minijinja template against a raw `serde_json::Value` (must be an Object).
///
/// Used by call sites in `main.rs` that already hold a `serde_json::Value`.
pub fn render_fmt_template_json(
    template: &str,
    data: &serde_json::Value,
) -> Result<String, String> {
    let env = build_env();
    env.render_str(template, data).map_err(|e| e.to_string())
}

/// Extract `(provider, field)` pairs from an eval template.
///
/// Scans for `{{ ident.ident ... }}` patterns using a simple char-level
/// scanner — no regex dependency. Returns pairs like `("git", "branch")`.
/// Unknown or unconstrained expressions (e.g., bare `{{ name }}` without a
/// dot) are silently ignored; they will resolve against whatever context is
/// provided at render time.
pub fn find_eval_template_pairs(template: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let bytes = template.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i + 1 < len {
        // Find "{{"
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            i += 2;
            // Skip whitespace
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            // Read first identifier
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let provider = &template[start..i];
            if provider.is_empty() {
                continue;
            }
            // Skip whitespace
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            // Expect a dot
            if i >= len || bytes[i] != b'.' {
                continue;
            }
            i += 1;
            // Read second identifier
            let start2 = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let field = &template[start2..i];
            if field.is_empty() {
                continue;
            }
            // We have a provider.field — record it (dedup via the caller)
            pairs.push((provider.to_string(), field.to_string()));
        } else {
            i += 1;
        }
    }

    pairs
}

/// Return the set of top-level provider names referenced in an eval template.
///
/// e.g. `"{{ git.branch }} {{ hostname.value }}"` → `{"git", "hostname"}`.
pub fn find_eval_template_refs(template: &str) -> HashSet<String> {
    find_eval_template_pairs(template)
        .into_iter()
        .map(|(provider, _field)| provider)
        .collect()
}

/// Render an eval template against a pre-built nested context.
///
/// The context must be a JSON object of the form
/// `{ "provider": { "field": value, ... }, ... }`.
///
/// Templates use `{{ provider.field }}` double-brace syntax with full
/// minijinja support: conditionals, filters (`truncate`, `default`, …), etc.
/// Single-brace `{provider.field}` is treated as a literal string.
pub fn render_eval_template(
    template: &str,
    context: &serde_json::Value,
) -> Result<String, String> {
    let env = build_env();
    env.render_str(template, context).map_err(|e| e.to_string())
}

fn truncate_filter(value: String, length: u32) -> String {
    if value.chars().count() <= length as usize {
        value
    } else {
        let mut s: String = value.chars().take(length as usize).collect();
        s.push_str("...");
        s
    }
}

fn prov_value_to_json(v: &ProvValue) -> serde_json::Value {
    match v {
        ProvValue::String(s) => serde_json::Value::String(s.clone()),
        ProvValue::Int(i) => serde_json::json!(*i),
        ProvValue::Bool(b) => serde_json::Value::Bool(*b),
        ProvValue::Float(f) => serde_json::json!(*f),
        ProvValue::Object(o) => {
            let map: serde_json::Map<_, _> = o
                .iter()
                .map(|(k, v)| (k.clone(), prov_value_to_json(v)))
                .collect();
            serde_json::Value::Object(map)
        }
    }
}
