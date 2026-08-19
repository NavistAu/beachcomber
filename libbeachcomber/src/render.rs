//! Client-side rendering of `get`/`watch` response data into the plain-text
//! and shell-export views the daemon used to render server-side, before
//! Task 1.8 retired the wire-level `format` sub-protocol.
//!
//! Everything on the wire is NDJSON now; `text`/`sh` rendering happens here,
//! after the JSON response has been parsed, instead of on the wire. This is
//! a faithful port of `format_data`'s `Format::Text` and `Format::Sh` arms
//! from `src/server.rs` (develop, pre-1.8) — the two rendered identically
//! there (no shell-quoting of any kind), so one function covers both.
//!
//! The wire form additionally framed each response with a trailing blank
//! line (and an empty object needed a special case, fixed in `63f8e55`, to
//! avoid emitting a stray extra one). Neither is relevant to a value that is
//! never going back on the wire: an empty object's line list is simply
//! empty, so `lines.join("\n")` already yields `""` with no special case
//! needed.

use serde_json::Value;

/// Render a `get`/`watch` response's `data` field the way the retired wire
/// text/sh sub-protocol did: scalars bare, objects as sorted
/// `subkey=value` lines with one level of `outer.inner=value` flattening for
/// nested objects, and `None`/`Value::Null` (a miss, or an explicit null)
/// both rendering as the empty string.
pub fn render_data(data: Option<&Value>) -> String {
    match data {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Object(map)) => {
            let mut lines: Vec<String> = map
                .iter()
                .flat_map(|(k, v)| {
                    if let Value::Object(inner) = v {
                        // Nested object: flatten as outer.inner=value
                        inner
                            .iter()
                            .map(|(ik, iv)| {
                                let val = match iv {
                                    Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                };
                                format!("{k}.{ik}={val}")
                            })
                            .collect::<Vec<_>>()
                    } else {
                        let val = match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        vec![format!("{k}={val}")]
                    }
                })
                .collect();
            lines.sort();
            lines.join("\n")
        }
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_string_is_bare() {
        assert_eq!(render_data(Some(&Value::String("main".into()))), "main");
    }

    #[test]
    fn scalar_number_is_bare() {
        assert_eq!(render_data(Some(&serde_json::json!(42))), "42");
    }

    #[test]
    fn scalar_bool_is_bare() {
        assert_eq!(render_data(Some(&Value::Bool(true))), "true");
    }

    #[test]
    fn missing_and_null_render_empty() {
        assert_eq!(render_data(None), "");
        assert_eq!(render_data(Some(&Value::Null)), "");
    }

    #[test]
    fn empty_object_renders_empty() {
        assert_eq!(render_data(Some(&serde_json::json!({}))), "");
    }

    #[test]
    fn object_renders_sorted_key_value_lines() {
        let data = serde_json::json!({"z": "last", "a": "first"});
        assert_eq!(render_data(Some(&data)), "a=first\nz=last");
    }

    #[test]
    fn nested_object_flattens_one_level() {
        let data = serde_json::json!({"tools": {"node": "20", "python": "3.12"}});
        assert_eq!(render_data(Some(&data)), "tools.node=20\ntools.python=3.12");
    }
}
