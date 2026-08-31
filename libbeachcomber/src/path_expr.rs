//! Path expressions: compute a query's cache-key path from a client-side jinja
//! expression over `cwd` and `env.*`. Empty/falsy result ⇒ global (no path).
//!
//! A path expression is written in the same three forms a value expression is
//! (canon `field_resolution.md` §"Path resolution" and §"`env.*` namespace",
//! which says `env.*` is available "in path expressions (`{{ env.X }}`)"):
//! bare, exactly one `{{ }}` tag, or a template. [`crate::eval`] owns the
//! classification, so `path = "env.KUBECONFIG or '~/.kube/config'"` and
//! `path = "{{ env.KUBECONFIG or '~/.kube/config' }}"` are the same expression.
//! The result is always a path string (or none), so the single-tag form's
//! type preservation is not observable here — only that both compile.

use crate::eval::{Form, classify, render_template, single_tag_expression};
use crate::virtual_fields::build_expression_env;
use minijinja::value::Value as MjValue;
use std::collections::HashMap;

const PATH_EXPRESSIONS: &[(&str, &str)] = &[
    ("kubecontext", "env.KUBECONFIG or '~/.kube/config'"),
    ("talos", "env.TALOSCONFIG or '~/.talos/config'"),
];

/// Config override wins over built-in default. None ⇒ provider declares none.
pub fn path_expression_for(provider: &str, overrides: &HashMap<String, String>) -> Option<String> {
    if let Some(e) = overrides.get(provider) {
        return Some(e.clone());
    }
    PATH_EXPRESSIONS
        .iter()
        .find(|(p, _)| *p == provider)
        .map(|(_, e)| e.to_string())
}

/// Evaluate a path expression in any of the three [`Form`]s. `cwd` + `env.*`
/// in scope. Empty/falsy ⇒ None.
/// A leading `~` in each ':'-separated component is expanded against $HOME.
///
/// The bare and single-tag forms compile as an expression and are stringified;
/// a template renders through [`render_template`]. Either way an empty or falsy
/// result is `None` — the global slot.
///
/// A source that fails to compile is `None` too, indistinguishable from a
/// deliberate fall-through to global. That is a known wart, queued in
/// `docs/roadmap.md`.
pub fn evaluate_path(expr: &str, cwd: &str, env_vars: &HashMap<String, String>) -> Option<String> {
    let mut ctx = serde_json::Map::new();
    ctx.insert("cwd".into(), serde_json::Value::String(cwd.to_string()));
    let env_obj: serde_json::Map<String, serde_json::Value> = env_vars
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    ctx.insert("env".into(), serde_json::Value::Object(env_obj));
    let ctx = serde_json::Value::Object(ctx);

    let s = match classify(expr) {
        // `single_tag_expression` yields the tag body for the single-tag form
        // and `None` for the bare one, where the whole source is the expression.
        Form::Expression | Form::SingleTag => {
            let inner = single_tag_expression(expr).unwrap_or(expr);
            let env = build_expression_env();
            let compiled = env.compile_expression(inner).ok()?;
            let result = compiled.eval(MjValue::from_serialize(&ctx)).ok()?;
            // Check the falsy variants before stringifying: minijinja's `none`
            // stringifies to "none" (not ""), so the empty check alone would
            // miss it.
            if result.is_undefined() || result.is_none() {
                return None;
            }
            result.to_string()
        }
        // `render_template` already renders a `none` as the empty string, so
        // the emptiness check below covers the falsy cases here.
        Form::Template => render_template(expr, &ctx).ok()?,
    };
    if s.is_empty() {
        return None;
    }
    Some(expand_tilde(&s, env_vars))
}

fn expand_tilde(path: &str, env_vars: &HashMap<String, String>) -> String {
    let Some(home) = env_vars.get("HOME").filter(|h| !h.is_empty()) else {
        return path.to_string();
    };
    path.split(':')
        .map(|c| {
            if let Some(r) = c.strip_prefix("~/") {
                format!("{home}/{r}")
            } else if c == "~" {
                home.clone()
            } else {
                c.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(":")
}
