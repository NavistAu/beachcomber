//! Client-side rendering of `get`/`watch` response data into the plain-text
//! and shell-export views the daemon used to render server-side, before
//! Task 1.8 retired the wire-level `format` sub-protocol.
//!
//! Everything on the wire is NDJSON now; `text`/`sh` rendering happens here,
//! after the JSON response has been parsed, instead of on the wire. This is
//! a port of `format_data`'s `Format::Text` and `Format::Sh` arms from
//! `src/server.rs` (develop, pre-1.8), which rendered identically there — no
//! shell-quoting of any kind. That was wrong: canon `field_resolution.md`
//! invariant 8 says `sh` shell-escapes, so a value carrying a space, a
//! single quote, a `$` or a newline is safe to `eval`. [`render_data`] now
//! takes the format and quotes under `sh`; [`shell_quote`] is the single
//! escaping helper, shared with the CLI's client-side virtual-field
//! rendering so the two paths cannot drift.
//!
//! The wire form additionally framed each response with a trailing blank
//! line (and an empty object needed a special case, fixed in `63f8e55`, to
//! avoid emitting a stray extra one). Neither is relevant to a value that is
//! never going back on the wire: an empty object's line list is simply
//! empty, so `lines.join("\n")` already yields `""` with no special case
//! needed.

use serde_json::Value;

/// POSIX single-quote a string: wrap in `'...'`, rewriting each internal `'`
/// as `'\''`. The one shell-escaping helper behind `--format sh` — used here
/// for daemon-backed values and by the CLI for client-side virtual fields.
pub fn shell_quote(s: &str) -> String {
    let escaped = s.replace('\'', r"'\''");
    format!("'{escaped}'")
}

/// Stringify one value the way `text` does — strings bare, everything else as
/// its JSON text — then shell-quote it when rendering for `sh`.
fn render_value(v: &Value, sh: bool) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if sh { shell_quote(&s) } else { s }
}

/// Render a `get`/`watch` response's `data` field into `format`: scalars bare,
/// objects as sorted `subkey=value` lines with one level of
/// `outer.inner=value` flattening for nested objects, and `None`/`Value::Null`
/// (a miss, or an explicit null) both rendering as the empty string.
///
/// `format` is the wire format name. `"sh"` shell-quotes every rendered value
/// — the scalar, or each value of a flattened object — so the output is safe
/// to `eval` (canon `field_resolution.md` invariant 8); keys are left bare.
/// Every other name (`"text"`, `"json"`, anything unrecognised) renders
/// unquoted, exactly as before. A miss stays the empty string under `sh` too:
/// nothing to assign, not an empty-string assignment.
pub fn render_data(data: Option<&Value>, format: &str) -> String {
    let sh = format == "sh";
    match data {
        Some(Value::Object(map)) => {
            let mut lines: Vec<String> = map
                .iter()
                .flat_map(|(k, v)| {
                    if let Value::Object(inner) = v {
                        // Nested object: flatten as outer.inner=value
                        inner
                            .iter()
                            .map(|(ik, iv)| format!("{k}.{ik}={}", render_value(iv, sh)))
                            .collect::<Vec<_>>()
                    } else {
                        vec![format!("{k}={}", render_value(v, sh))]
                    }
                })
                .collect();
            lines.sort();
            lines.join("\n")
        }
        Some(Value::Null) | None => String::new(),
        Some(other) => render_value(other, sh),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_string_is_bare() {
        assert_eq!(
            render_data(Some(&Value::String("main".into())), "text"),
            "main"
        );
    }

    #[test]
    fn scalar_number_is_bare() {
        assert_eq!(render_data(Some(&serde_json::json!(42)), "text"), "42");
    }

    #[test]
    fn scalar_bool_is_bare() {
        assert_eq!(render_data(Some(&Value::Bool(true)), "text"), "true");
    }

    #[test]
    fn missing_and_null_render_empty() {
        assert_eq!(render_data(None, "text"), "");
        assert_eq!(render_data(Some(&Value::Null), "text"), "");
    }

    #[test]
    fn empty_object_renders_empty() {
        assert_eq!(render_data(Some(&serde_json::json!({})), "text"), "");
    }

    #[test]
    fn object_renders_sorted_key_value_lines() {
        let data = serde_json::json!({"z": "last", "a": "first"});
        assert_eq!(render_data(Some(&data), "text"), "a=first\nz=last");
    }

    #[test]
    fn nested_object_flattens_one_level() {
        let data = serde_json::json!({"tools": {"node": "20", "python": "3.12"}});
        assert_eq!(
            render_data(Some(&data), "text"),
            "tools.node=20\ntools.python=3.12"
        );
    }

    // ── sh: canon field_resolution.md invariant 8 ────────────────────────────

    #[test]
    fn shell_quote_wraps_and_escapes_inner_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn sh_quotes_scalars() {
        assert_eq!(
            render_data(Some(&Value::String("it's $HOME".into())), "sh"),
            r"'it'\''s $HOME'"
        );
        assert_eq!(render_data(Some(&serde_json::json!(42)), "sh"), "'42'");
        assert_eq!(render_data(Some(&Value::Bool(true)), "sh"), "'true'");
    }

    #[test]
    fn sh_quotes_every_object_value_keys_stay_bare() {
        let data = serde_json::json!({"z": "two words", "a": "it's"});
        assert_eq!(
            render_data(Some(&data), "sh"),
            "a='it'\\''s'\nz='two words'"
        );
    }

    #[test]
    fn sh_quotes_flattened_nested_values() {
        let data = serde_json::json!({"tools": {"node": "20 rc", "python": "3.12"}});
        assert_eq!(
            render_data(Some(&data), "sh"),
            "tools.node='20 rc'\ntools.python='3.12'"
        );
    }

    #[test]
    fn sh_miss_stays_empty_not_empty_quotes() {
        assert_eq!(render_data(None, "sh"), "");
        assert_eq!(render_data(Some(&Value::Null), "sh"), "");
        assert_eq!(render_data(Some(&serde_json::json!({})), "sh"), "");
    }

    #[test]
    fn unrecognised_format_renders_like_text() {
        let data = serde_json::json!({"a": "it's"});
        assert_eq!(render_data(Some(&data), "json"), "a=it's");
        assert_eq!(render_data(Some(&data), "csv"), "a=it's");
    }
}
