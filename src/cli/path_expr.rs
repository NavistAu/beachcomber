//! Path expressions: compute a query's cache-key path from a client-side jinja
//! expression over `cwd` and `env.*`. Empty/falsy result ⇒ global (no path).

use crate::cli::virtual_fields::build_expression_env;
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

/// Evaluate a path expression. `cwd` + `env.*` in scope. Empty/falsy ⇒ None.
/// A leading `~` in each ':'-separated component is expanded against $HOME.
pub fn evaluate_path(expr: &str, cwd: &str, env_vars: &HashMap<String, String>) -> Option<String> {
    let mut ctx = serde_json::Map::new();
    ctx.insert("cwd".into(), serde_json::Value::String(cwd.to_string()));
    let env_obj: serde_json::Map<String, serde_json::Value> = env_vars
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    ctx.insert("env".into(), serde_json::Value::Object(env_obj));
    let env = build_expression_env();
    let compiled = env.compile_expression(expr).ok()?;
    let result = compiled
        .eval(MjValue::from_serialize(serde_json::Value::Object(ctx)))
        .ok()?;
    let s = result.to_string();
    if s.is_empty() || result.is_undefined() || result.is_none() {
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
