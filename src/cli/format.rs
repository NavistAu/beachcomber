use serde_json;

use crate::provider::{ProviderResult, Value as ProvValue};

/// Render a `-f fmt` template against a `ProviderResult`.
///
/// Templates use `{{ field }}` double-brace syntax. Filters available:
/// - `truncate(N)` — first N chars + "..." if longer (custom; not in minijinja builtins)
/// - All standard minijinja `builtins` filters: `default`, `upper`, `lower`, `trim`, etc.
/// - Conditionals: `{% if dirty %}*{% endif %}`
///
/// Single-brace `{field}` is treated as a literal string (no substitution).
pub fn render_fmt_template(template: &str, result: &ProviderResult) -> Result<String, String> {
    let ctx: serde_json::Map<String, serde_json::Value> = result
        .fields
        .iter()
        .map(|(k, v)| (k.clone(), prov_value_to_json(v)))
        .collect();

    render_fmt_template_json(template, &serde_json::Value::Object(ctx))
}

/// Render a `-f fmt` template against a raw `serde_json::Value` (must be an Object).
///
/// Used by call sites that already hold a `serde_json::Value`.
///
/// The render itself is [`libbeachcomber::eval::render_template`] — the
/// workspace's one template render, shared with value-expression evaluation, so
/// `-f fmt`, `comb eval` and a config `virtual.x` see the same filters and the
/// same undefined/`none` handling.
pub fn render_fmt_template_json(
    template: &str,
    data: &serde_json::Value,
) -> Result<String, String> {
    libbeachcomber::eval::render_template(template, data)
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
