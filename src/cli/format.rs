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
/// Scans for `ident.ident` patterns inside minijinja tag boundaries using a
/// simple byte-level state machine — no regex dependency. Returns pairs like
/// `("git", "branch")`.
///
/// Tag handling:
/// - `{{ ... }}` — expression tags: scan for the first `ident.ident` after
///   the opening braces (the existing behaviour).
/// - `{% ... %}` and `{%- ... -%}` — block tags: scan the entire block body
///   for all `ident.ident` occurrences. Handles whitespace-control dashes.
/// - `{# ... #}` — comment tags: skipped entirely; no scanning.
///
/// Within block tags, simple string-literal tracking skips content inside
/// `"..."` and `'...'` so that a literal like `{% if x == "foo.bar" %}` does
/// not produce a spurious `("foo", "bar")` pair. Escape sequences (`\"`, `\'`)
/// are handled minimally: a backslash advances past the next byte, which is
/// sufficient to avoid being fooled by `"\""` ending a string early.
///
/// Bare identifiers without a dot (e.g., `{{ name }}`, `{% for x in list %}`)
/// are silently ignored; they will resolve against whatever context is provided
/// at render time.
pub fn find_eval_template_pairs(template: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let bytes = template.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i + 1 < len {
        if bytes[i] == b'{' {
            match bytes[i + 1] {
                // Expression tag: {{ ... }}
                // Scan for the first ident.ident only (existing behaviour).
                b'{' => {
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
                }

                // Block tag: {% ... %} or {%- ... -%}
                // Scan the entire block body for all ident.ident patterns.
                b'%' => {
                    i += 2;
                    // Consume optional whitespace-control dash: {%-
                    if i < len && bytes[i] == b'-' {
                        i += 1;
                    }
                    // Walk the block body until closing %} or -%}
                    let mut in_string: Option<u8> = None; // Some(b'"') or Some(b'\'')
                    while i + 1 < len {
                        // Detect closing: -%} or %}
                        let closing = (bytes[i] == b'-' && bytes[i + 1] == b'%'
                            && i + 2 < len && bytes[i + 2] == b'}')
                            || (bytes[i] == b'%' && bytes[i + 1] == b'}');
                        if closing {
                            // Advance past the closing delimiter
                            if bytes[i] == b'-' {
                                i += 3; // -%}
                            } else {
                                i += 2; // %}
                            }
                            break;
                        }

                        if let Some(delim) = in_string {
                            // Inside a string literal
                            if bytes[i] == b'\\' {
                                // Skip escaped byte (handles \", \', \\, etc.)
                                i += 2;
                            } else if bytes[i] == delim {
                                // End of string literal
                                in_string = None;
                                i += 1;
                            } else {
                                i += 1;
                            }
                        } else if bytes[i] == b'"' || bytes[i] == b'\'' {
                            // Start of string literal
                            in_string = Some(bytes[i]);
                            i += 1;
                        } else if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
                            // Potential ident.ident — try to read it
                            let start = i;
                            while i < len
                                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                            {
                                i += 1;
                            }
                            // Skip any whitespace between ident and dot
                            // (minijinja does allow `ident . ident` but it's unusual;
                            //  we only capture the no-whitespace form to stay conservative)
                            if i < len && bytes[i] == b'.' {
                                let provider = &template[start..i];
                                i += 1; // consume the dot
                                let start2 = i;
                                while i < len
                                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                                {
                                    i += 1;
                                }
                                let field = &template[start2..i];
                                if !provider.is_empty() && !field.is_empty() {
                                    pairs.push((provider.to_string(), field.to_string()));
                                }
                            }
                            // else: bare identifier, no dot — skip (already advanced past it)
                        } else {
                            i += 1;
                        }
                    }
                }

                // Comment tag: {# ... #}  — skip entirely, do not scan.
                b'#' => {
                    i += 2;
                    while i + 1 < len {
                        if bytes[i] == b'#' && bytes[i + 1] == b'}' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }

                // Lone `{` that doesn't start a recognized tag — advance.
                _ => {
                    i += 1;
                }
            }
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
