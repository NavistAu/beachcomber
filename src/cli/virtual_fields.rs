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
        "env.CLOUDSDK_CORE_PROJECT or gcloud.config_project",
    ),
];

// ── Types ─────────────────────────────────────────────────────────────────────

/// A kinded reference discovered in an expression.
///
/// Three reference kinds per the field_resolution.md canonical spec:
/// - `env.X` → `Env(X)`
/// - `cache.P.F` → `CacheField(P, F)` (raw cached value, bypasses field expressions)
/// - `cache.P` → `CacheProvider(P)` (the whole provider object)
/// - `P.F` (P ∉ {env, cache}) → `Resolved(P, F)` (resolved field, recurse if virtual)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ref {
    /// `env.X` — the calling shell's environment variable X.
    Env(String),
    /// `cache.P.F` — the raw cached field value, bypassing field expressions.
    CacheField(String, String),
    /// `cache.P` — the whole provider object from the cache.
    CacheProvider(String),
    /// `P.F` — the resolved field value (recursive if virtual, daemon fetch if not).
    Resolved(String, String),
}

/// The evaluation context: resolved env vars + pre-fetched daemon data.
pub struct EvalContext<'a> {
    /// The calling shell's environment variables.
    pub env_vars: &'a HashMap<String, String>,
    /// Pre-fetched daemon values:
    /// - key = "provider.field" for individual fields
    /// - key = "provider" for whole-provider objects (CacheProvider refs)
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

    /// Returns the list of virtual fields defined for a provider.
    ///
    /// Used for whole-namespace evaluation: if `fields_for(P)` is non-empty,
    /// a bare `comb get P` evaluates each virtual field and assembles an object.
    pub fn fields_for(&self, provider: &str) -> Vec<String> {
        let mut fields: Vec<String> = self
            .fields
            .keys()
            .filter(|(p, _)| p == provider)
            .map(|(_, f)| f.clone())
            .collect();
        fields.sort();
        fields
    }

    /// Serialize the built-in virtual fields to a TOML config snippet.
    /// Groups entries by provider: `[providers.<name>]` with `virtual.<field> = "expression"` keys.
    /// Uses TOML's dotted-key form: `virtual.<field>` under `[providers.<name>]`.
    pub fn to_config_toml(&self) -> String {
        use std::collections::BTreeMap;

        // Sort by provider for stable output.
        let mut by_provider: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
        for ((provider, field), expr) in &self.fields {
            by_provider.entry(provider).or_default().push((field, expr));
        }

        let mut out = String::new();
        out.push_str(
            "# Virtual fields (expression form) — generated by comb init --write-config\n",
        );
        out.push_str("# Edit expressions to customize cascade order.\n");
        out.push_str(
            "# TOML key 'virtual.<field>' namespaces expressions under the virtual sub-table.\n\n",
        );
        for (provider, mut fields) in by_provider {
            fields.sort_by_key(|(f, _)| *f);
            out.push_str(&format!("[providers.{}]\n", provider));
            for (field, expr) in fields {
                // TOML dotted-key form: virtual.<field> = "<expression>"
                // "virtual" is read as a string key in TOML — no Rust keyword collision.
                let escaped = expr.replace('\\', "\\\\").replace('"', "\\\"");
                out.push_str(&format!("virtual.{} = \"{}\"\n", field, escaped));
            }
            out.push('\n');
        }
        out
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
    ///
    /// - `Env(X)` refs → `ctx.env_vars.get(X)` (empty string on miss).
    /// - `CacheField(P, F)` refs → `ctx.daemon_data["P.F"]` (raw cached value).
    /// - `CacheProvider(P)` refs → `ctx.daemon_data["P"]` (whole provider object).
    /// - `Resolved(P, F)` refs → if virtual, recurse `self.evaluate(P, F, ...)`
    ///   with cycle detection; otherwise `ctx.daemon_data["P.F"]`.
    pub fn evaluate_expression(
        &self,
        expr: &str,
        ctx: &EvalContext<'_>,
        stack: &mut HashSet<(String, String)>,
    ) -> Result<JsonValue, String> {
        let refs = discover_expression_refs(expr);
        let ctx_json = build_context_json(&refs, ctx, self, stack)?;
        let top = MjValue::from_serialize(&ctx_json);

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

/// Evaluate all virtual fields for a provider and return them as a JSON object.
///
/// Fields that fail to evaluate (e.g., cycle errors) are silently omitted.
/// Callers may merge additional daemon-backed fields on top.
pub fn evaluate_namespace(
    provider: &str,
    vf: &VirtualFields,
    env_vars: &HashMap<String, String>,
    daemon_data: &HashMap<String, JsonValue>,
) -> JsonValue {
    let ctx = EvalContext {
        env_vars,
        daemon_data,
    };
    let mut obj = serde_json::Map::new();
    for field in vf.fields_for(provider) {
        let mut stack = HashSet::new();
        match vf.evaluate(provider, &field, &ctx, &mut stack) {
            Ok(v) => {
                obj.insert(field, v);
            }
            Err(_) => {
                // Silently omit fields that error (e.g., cycle errors, missing deps).
            }
        }
    }
    JsonValue::Object(obj)
}

/// Build a serde_json context object from the discovered refs and the eval context.
///
/// Assembles a nested object:
/// - `env` ← `{X: env_vars.get(X) || ""}` for each `Env(X)`
/// - `cache` ← `{P: {F: daemon_data["P.F"]}}` for each `CacheField(P, F)`
///   and `{P: daemon_data["P"]}` for each `CacheProvider(P)`
/// - Top-level `P: {F: <resolved>}` for each `Resolved(P, F)`
///   (if `vf.is_virtual(P, F)` → recurse `vf.evaluate(...)`; else `daemon_data["P.F"]`)
fn build_context_json(
    refs: &[Ref],
    ctx: &EvalContext<'_>,
    vf: &VirtualFields,
    stack: &mut HashSet<(String, String)>,
) -> Result<JsonValue, String> {
    let mut top: serde_json::Map<String, JsonValue> = serde_json::Map::new();

    // Ensure env namespace always present (even if empty).
    top.insert("env".to_string(), JsonValue::Object(serde_json::Map::new()));

    // Ensure cache namespace always present.
    top.insert(
        "cache".to_string(),
        JsonValue::Object(serde_json::Map::new()),
    );

    for r in refs {
        match r {
            Ref::Env(var) => {
                let val = ctx.env_vars.get(var).cloned().unwrap_or_default();
                // Insert into the pre-created env namespace.
                let env_entry = top
                    .entry("env".to_string())
                    .or_insert_with(|| JsonValue::Object(serde_json::Map::new()));
                if let JsonValue::Object(env_map) = env_entry {
                    env_map.insert(var.clone(), JsonValue::String(val));
                }
            }

            Ref::CacheField(provider, field) => {
                let raw_val = ctx
                    .daemon_data
                    .get(&format!("{provider}.{field}"))
                    .cloned()
                    .unwrap_or(JsonValue::Null);
                let cache_entry = top
                    .entry("cache".to_string())
                    .or_insert_with(|| JsonValue::Object(serde_json::Map::new()));
                if let JsonValue::Object(cache_map) = cache_entry {
                    let provider_entry = cache_map
                        .entry(provider.clone())
                        .or_insert_with(|| JsonValue::Object(serde_json::Map::new()));
                    if let JsonValue::Object(pmap) = provider_entry {
                        pmap.insert(field.clone(), raw_val);
                    }
                }
            }

            Ref::CacheProvider(provider) => {
                let whole_obj = ctx
                    .daemon_data
                    .get(provider.as_str())
                    .cloned()
                    .unwrap_or(JsonValue::Null);
                let cache_entry = top
                    .entry("cache".to_string())
                    .or_insert_with(|| JsonValue::Object(serde_json::Map::new()));
                if let JsonValue::Object(cache_map) = cache_entry {
                    cache_map.insert(provider.clone(), whole_obj);
                }
            }

            Ref::Resolved(provider, field) => {
                let resolved_val = if vf.is_virtual(provider, field) {
                    vf.evaluate(provider, field, ctx, stack)?
                } else {
                    ctx.daemon_data
                        .get(&format!("{provider}.{field}"))
                        .cloned()
                        .unwrap_or(JsonValue::Null)
                };
                let provider_entry = top
                    .entry(provider.clone())
                    .or_insert_with(|| JsonValue::Object(serde_json::Map::new()));
                if let JsonValue::Object(pmap) = provider_entry {
                    pmap.insert(field.clone(), resolved_val);
                }
            }
        }
    }

    Ok(JsonValue::Object(top))
}

// ── Ref discovery ─────────────────────────────────────────────────────────────

/// Discover all refs in an expression using minijinja's
/// `Expression::undeclared_variables(true)` (nested = true).
///
/// Classifies each dotted name:
/// - `env.X` → `Ref::Env(X)`
/// - `cache.P.F` → `Ref::CacheField(P, F)`
/// - `cache.P` → `Ref::CacheProvider(P)`
/// - `cwd` → ignored (path-expression variable, reserved for a later task)
/// - `P.F` (P ∉ {env, cache, cwd}) → `Ref::Resolved(P, F)`
/// - bare single name → ignored
///
/// Returns a deduplicated list of refs.
pub fn discover_expression_refs(expr: &str) -> Vec<Ref> {
    let env = build_expression_env();
    let Ok(compiled) = env.compile_expression(expr) else {
        return vec![];
    };
    // VERIFIED against vendor/minijinja 2.19.0 (compiler/meta.rs:141-157): with
    // nested = true, an attribute chain `a.b` is recorded as the dotted string "a.b"
    // (e.g. `env.PYENV_VERSION or mise.python` → {"env.PYENV_VERSION", "mise.python"}).
    // So we split each entry on the first '.' — no byte-scanning needed.
    let mut refs: Vec<Ref> = Vec::new();
    let mut seen: HashSet<Ref> = HashSet::new();

    for v in compiled.undeclared_variables(true) {
        let mut segments = v.splitn(4, '.');
        let first = match segments.next() {
            Some(s) => s,
            None => continue,
        };
        let second = match segments.next() {
            Some(s) => s,
            None => {
                // Bare name — skip (includes "cwd").
                continue;
            }
        };
        // Segments beyond the second are MiniJinja attribute navigation into
        // the fetched value — NOT part of the ref key itself.
        let third = segments.next(); // optional: None for two-segment refs

        let r = match first {
            "env" => Ref::Env(second.to_string()),
            "cache" => match third {
                Some(_) => Ref::CacheField(second.to_string(), third.unwrap().to_string()),
                None => Ref::CacheProvider(second.to_string()),
            },
            "cwd" => {
                // Path-expression variable — reserved for a later task; ignore here.
                continue;
            }
            _ => {
                // P.F where P ∉ {env, cache, cwd} → Resolved(P, F)
                Ref::Resolved(first.to_string(), second.to_string())
            }
        };

        if seen.insert(r.clone()) {
            refs.push(r);
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
        if let Ok(n) = v.clone().try_into() as Result<u64, _> {
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
