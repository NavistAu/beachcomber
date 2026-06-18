//! Client-side virtual field evaluator.
//!
//! A virtual field (expression form) is a minijinja *expression* (not a template)
//! evaluated to a typed `serde_json::Value`. Ref discovery uses
//! `Expression::undeclared_variables(true)` to enumerate all `provider.field`
//! and `env.*` refs in the expression — no byte-level scanning.
//!
//! Built-in default virtual fields are compiled into the CLI; no config file
//! is required. Config may override or extend them.
//!
//! RUST NOTE: `virtual` is a reserved keyword in Rust. This module is named
//! `virtual_fields`. The TOML key "virtual" is read as a string literal
//! `"virtual"` in Rust code — e.g. `table.get("virtual")` — which is legal.

use minijinja::value::Value as MjValue;
use minijinja::{Environment, UndefinedBehavior};
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};

// ── Built-in default virtual fields (expression form) ────────────────────────

/// Built-in virtual field definitions, compiled into the CLI.
/// Format: (provider, field, expression).
/// Config overrides win over these defaults.
const BUILTIN_DEFAULTS: &[(&str, &str, &str)] = &[
    // terraform
    (
        "terraform",
        "workspace",
        "env.TF_WORKSPACE or terraform.path_workspace",
    ),
    // python
    (
        "python",
        "version",
        "env.PYENV_VERSION or env.MISE_PYTHON_VERSION or mise.python or asdf.python or python.venv_version",
    ),
    (
        "python",
        "venv_name",
        r#"python.local_venv_name or (env.VIRTUAL_ENV | basename)"#,
    ),
    // conda (daemon provider removed; virtual from env only)
    ("conda", "env", "env.CONDA_DEFAULT_ENV"),
    // aws
    (
        "aws",
        "profile",
        "env.AWS_PROFILE or env.AWS_VAULT or env.AWS_DEFAULT_PROFILE",
    ),
    (
        "aws",
        "region",
        "env.AWS_REGION or env.AWS_DEFAULT_REGION or aws.config_region",
    ),
    (
        "aws",
        "expiration",
        "env.AWS_CREDENTIAL_EXPIRATION or env.AWS_SESSION_EXPIRATION",
    ),
    // op (daemon provider removed; virtual from env only)
    // Security: expression returns a bool, NOT the token string.
    ("op", "signed_in", r#"env.OP_SERVICE_ACCOUNT_TOKEN != """#),
    // gcloud (P1: env direct values only; live.* override is P2)
    (
        "gcloud",
        "project",
        "env.CLOUDSDK_CORE_PROJECT or gcloud.project",
    ),
];

// ── Types ─────────────────────────────────────────────────────────────────────

/// A (provider, field) pair from expression ref discovery.
pub type FieldRef = (String, String);

/// The evaluation context: resolved env vars + pre-fetched daemon data.
pub struct EvalContext<'a> {
    /// The calling shell's environment variables.
    pub env_vars: &'a HashMap<String, String>,
    /// Pre-fetched daemon values: key = "provider.field", value = JSON value.
    pub daemon_data: &'a HashMap<String, JsonValue>,
}

/// The virtual field registry: built-in defaults + config overrides.
pub struct VirtualFields {
    /// (provider, field) → expression string.
    fields: HashMap<(String, String), String>,
}

impl VirtualFields {
    /// Build from built-in defaults only (no config file).
    pub fn defaults_only() -> Self {
        let mut fields = HashMap::new();
        for (provider, field, expr) in BUILTIN_DEFAULTS {
            fields.insert((provider.to_string(), field.to_string()), expr.to_string());
        }
        Self { fields }
    }

    /// Build from built-in defaults + config overrides.
    /// Config entries override built-in defaults when both define the same (provider, field).
    pub fn with_config_overrides(
        overrides: impl IntoIterator<Item = ((String, String), String)>,
    ) -> Self {
        let mut vf = Self::defaults_only();
        for (key, expr) in overrides {
            vf.fields.insert(key, expr);
        }
        vf
    }

    /// Returns true if `(provider, field)` is a virtual field (expression form).
    pub fn is_virtual(&self, provider: &str, field: &str) -> bool {
        self.fields
            .contains_key(&(provider.to_string(), field.to_string()))
    }

    /// Return the expression for a virtual field, if any.
    pub fn expression(&self, provider: &str, field: &str) -> Option<&str> {
        self.fields
            .get(&(provider.to_string(), field.to_string()))
            .map(|s| s.as_str())
    }

    /// Evaluate a virtual field. Returns the typed JSON value.
    ///
    /// `stack` tracks the current evaluation path for cycle detection.
    /// Callers should pass `&mut HashSet::new()` for the outermost call.
    pub fn evaluate(
        &self,
        provider: &str,
        field: &str,
        ctx: &EvalContext<'_>,
        stack: &mut HashSet<(String, String)>,
    ) -> Result<JsonValue, String> {
        let key = (provider.to_string(), field.to_string());
        if stack.contains(&key) {
            return Err(format!(
                "virtual field cycle detected: {provider}.{field} references itself"
            ));
        }
        let expr = self
            .expression(provider, field)
            .ok_or_else(|| format!("{provider}.{field} is not a virtual field"))?;
        stack.insert(key.clone());
        let result = self.evaluate_expression(expr, ctx, stack);
        stack.remove(&key);
        result
    }

    /// Evaluate an arbitrary expression string against the given context.
    ///
    /// Refs in the expression are discovered via `undeclared_variables(true)`.
    /// `env.*` refs are resolved from `ctx.env_vars`.
    /// `provider.field` refs that are themselves virtual fields are evaluated
    /// recursively (with cycle detection via `stack`).
    /// Other `provider.field` refs are looked up in `ctx.daemon_data`.
    pub fn evaluate_expression(
        &self,
        expr: &str,
        ctx: &EvalContext<'_>,
        stack: &mut HashSet<(String, String)>,
    ) -> Result<JsonValue, String> {
        // Build the minijinja context object from refs.
        // env.* → provided by env_vars (always present, empty string on miss).
        // provider.field → from daemon_data or recursive virtual evaluation.
        let refs = discover_expression_refs(expr);

        // Assemble a nested JSON context: { "env": { "FOO": "val" }, "provider": { "field": val }, ... }
        // Use serde_json serialization path — minijinja can deserialize a serde_json::Value.
        let ctx_json: serde_json::Value = build_context_json(&refs, ctx, self, stack)?;
        let top = MjValue::from_serialize(&ctx_json);

        // Evaluate the expression.
        let env = build_expression_env();
        let compiled = env
            .compile_expression(expr)
            .map_err(|e| format!("expression compile error: {e}"))?;
        let mj_result = compiled
            .eval(top)
            .map_err(|e| format!("expression eval error: {e}"))?;

        Ok(mj_to_json(mj_result))
    }
}

/// Build a serde_json context object from the discovered refs and the eval context.
fn build_context_json(
    refs: &[FieldRef],
    ctx: &EvalContext<'_>,
    vf: &VirtualFields,
    stack: &mut HashSet<(String, String)>,
) -> Result<serde_json::Value, String> {
    let mut top: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

    // env.* namespace
    let mut env_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for (p, f) in refs {
        if p == "env" {
            let val = ctx.env_vars.get(f).cloned().unwrap_or_default();
            env_map.insert(f.clone(), serde_json::Value::String(val));
        }
    }
    top.insert("env".to_string(), serde_json::Value::Object(env_map));

    // provider.field namespace
    for (p, f) in refs {
        if p == "env" {
            continue;
        }
        let json_key = format!("{p}.{f}");
        let json_val = if vf.is_virtual(p, f) {
            vf.evaluate(p, f, ctx, stack)?
        } else if let Some(v) = ctx.daemon_data.get(&json_key) {
            v.clone()
        } else {
            serde_json::Value::Null
        };

        let provider_entry = top
            .entry(p.clone())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let serde_json::Value::Object(m) = provider_entry {
            m.insert(f.clone(), json_val);
        }
    }

    Ok(serde_json::Value::Object(top))
}

// ── Ref discovery ─────────────────────────────────────────────────────────────

/// Discover all `provider.field` refs in an expression using minijinja's
/// `Expression::undeclared_variables(true)` (nested = true).
///
/// Returns a deduplicated list of `(provider, field)` pairs.
/// `env.FOO` → `("env", "FOO")`.
pub fn discover_expression_refs(expr: &str) -> Vec<FieldRef> {
    let env = build_expression_env();
    let Ok(compiled) = env.compile_expression(expr) else {
        return vec![];
    };
    // VERIFIED against vendor/minijinja 2.19.0 (compiler/meta.rs:141-157): with
    // nested = true, an attribute chain `a.b` is recorded as the dotted string "a.b"
    // (e.g. `env.PYENV_VERSION or mise.python` → {"env.PYENV_VERSION", "mise.python"}).
    // So we split each entry on the first '.' — no byte-scanning needed.
    let mut refs: Vec<FieldRef> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for v in compiled.undeclared_variables(true) {
        if let Some((provider, field)) = v.split_once('.') {
            let pair = (provider.to_string(), field.to_string());
            if seen.insert(pair.clone()) {
                refs.push(pair);
            }
        }
    }
    refs
}

// ── Minijinja environment for expressions ─────────────────────────────────────

/// Build a minijinja `Environment` suitable for `compile_expression`.
///
/// Registers the same filters as `build_env()` (truncate, basename) and
/// sets lenient undefined behavior so missing refs are falsy, not errors.
pub(crate) fn build_expression_env<'a>() -> Environment<'a> {
    use crate::cli::format::build_env;
    let mut env = build_env();
    env.set_undefined_behavior(UndefinedBehavior::Lenient);
    env
}

// ── Value conversion ──────────────────────────────────────────────────────────

/// Convert a minijinja `Value` to a `serde_json::Value`.
///
/// Preserves types: bool → bool, integer → number, string → string.
/// UNDEFINED / None → empty string (all-falsy result).
pub(crate) fn mj_to_json(v: MjValue) -> JsonValue {
    if v.is_undefined() || v.is_none() {
        return JsonValue::String(String::new());
    }
    // Try bool first (minijinja booleans are distinct from strings).
    if v.kind() == minijinja::value::ValueKind::Bool
        && let Ok(b) = v.clone().try_into() as Result<bool, _>
    {
        return JsonValue::Bool(b);
    }
    // Try integer.
    if v.kind() == minijinja::value::ValueKind::Number {
        if let Ok(n) = v.clone().try_into() as Result<i64, _> {
            return JsonValue::Number(serde_json::Number::from(n));
        }
        if let Ok(f) = v.clone().try_into() as Result<f64, _>
            && let Some(n) = serde_json::Number::from_f64(f)
        {
            return JsonValue::Number(n);
        }
    }
    // String fallback — also handles UNDEFINED that slipped through.
    let s = v.to_string();
    JsonValue::String(s)
}
