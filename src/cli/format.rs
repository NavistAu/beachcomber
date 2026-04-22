use minijinja::Environment;
use serde_json;

use crate::provider::{ProviderResult, Value as ProvValue};

/// Render a minijinja template against a `ProviderResult`.
///
/// Templates use `{{ field }}` double-brace syntax. Filters available:
/// - `truncate(N)` — first N chars + "..." if longer (custom; not in minijinja builtins)
/// - All standard minijinja `builtins` filters: `default`, `upper`, `lower`, `trim`, etc.
/// - Conditionals: `{% if dirty %}*{% endif %}`
///
/// Single-brace `{field}` is treated as a literal string (no substitution).
pub fn render_fmt_template(template: &str, result: &ProviderResult) -> Result<String, String> {
    let mut env = Environment::new();
    env.add_filter("truncate", truncate_filter);

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
    let mut env = Environment::new();
    env.add_filter("truncate", truncate_filter);

    env.render_str(template, data).map_err(|e| e.to_string())
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
