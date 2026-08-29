//! Client-side virtual field evaluator.
//!
//! A virtual field's value expression is written bare (`env.A or cache.x.y`),
//! as a single tag (`{{ env.A or cache.x.y }}`), or as a template
//! (`{{ git.branch }}{% if git.dirty %}*{% endif %}`). The first two evaluate
//! to a typed `serde_json::Value`; a template is string-valued. [`crate::eval`]
//! owns that classification and the evaluation itself; this module owns the
//! registry, the reference taxonomy, and the context the refs bind to. Ref
//! discovery uses minijinja's `undeclared_variables(true)` meta-analysis to
//! enumerate all `provider.field` and `env.*` refs — no byte-level scanning.
//!
//! Built-in default virtual fields are compiled into `libbeachcomber`; no
//! config file is required. Config may override or extend them. Any binary
//! that links this crate gets them — the `comb` CLI today, and a statusline
//! renderer plus C ABI / language SDK bindings in later phases.
//!
//! RUST NOTE: `virtual` is a reserved keyword in Rust. This module is named
//! `virtual_fields`. The TOML key "virtual" is read as a string literal
//! `"virtual"` in Rust code — e.g. `table.get("virtual")` — which is legal.

use minijinja::value::Value as MjValue;
use minijinja::{Environment, UndefinedBehavior};
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};

// ── Built-in default virtual fields (expression form) ────────────────────────

/// Built-in virtual field definitions, compiled into `libbeachcomber`.
/// Format: (provider, field, expression).
/// Config overrides win over these defaults.
const BUILTIN_DEFAULTS: &[(&str, &str, &str)] = &[
    // terraform
    (
        "terraform",
        "workspace",
        "env.TF_WORKSPACE or cache.terraform.workspace",
    ),
    // python
    (
        "python",
        "version",
        "env.PYENV_VERSION or env.MISE_PYTHON_VERSION or cache.mise.python or cache.asdf.python or cache.python.venv_version",
    ),
    (
        "python",
        "venv_name",
        r#"cache.python.local_venv_name or (env.VIRTUAL_ENV | basename)"#,
    ),
    // conda (daemon provider removed; virtual from env only)
    ("conda", "env", "env.CONDA_DEFAULT_ENV"),
    // op (daemon provider removed; virtual from env only)
    // Security: expression returns a bool, NOT the token string.
    ("op", "signed_in", r#"env.OP_SERVICE_ACCOUNT_TOKEN != """#),
    // aws
    (
        "aws",
        "profile",
        r#"env.AWS_PROFILE or env.AWS_VAULT or env.AWS_DEFAULT_PROFILE or "default""#,
    ),
    (
        "aws",
        "region",
        r#"env.AWS_REGION or env.AWS_DEFAULT_REGION or cache.aws_profiles[ env.AWS_PROFILE or env.AWS_VAULT or env.AWS_DEFAULT_PROFILE or "default" ].region"#,
    ),
    (
        "aws",
        "expiration",
        "env.AWS_CREDENTIAL_EXPIRATION or env.AWS_SESSION_EXPIRATION",
    ),
    // gcloud
    (
        "gcloud",
        "project",
        "env.CLOUDSDK_CORE_PROJECT or cache.gcloud_configs[ env.CLOUDSDK_ACTIVE_CONFIG_NAME or cache.gcloud_configs.active_config ].project",
    ),
    (
        "gcloud",
        "account",
        "cache.gcloud_configs[ env.CLOUDSDK_ACTIVE_CONFIG_NAME or cache.gcloud_configs.active_config ].account",
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
///
/// `Ord` is derived: variants sort in declaration order (Env < CacheField <
/// CacheProvider < Resolved), then field-wise. Discovery runs off minijinja's
/// `HashSet` of undeclared variables, so sorting is what makes ref order
/// reproducible across runs — see `crate::eval::discover_refs`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
    /// Groups entries by provider: `[providers.<name>]` with
    /// `virtual.<field> = "{{ expression }}"` keys.
    /// Uses TOML's dotted-key form: `virtual.<field>` under `[providers.<name>]`.
    ///
    /// Expressions are written in the documented single-tag form (canon
    /// `field_resolution.md` invariant 14), which reads back as the same typed
    /// field the bare form would. `BUILTIN_DEFAULTS` stay bare in the source:
    /// they are internal, and the bare form remains accepted permanently.
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
                // TOML dotted-key form: virtual.<field> = "{{ <expression> }}"
                // "virtual" is read as a string key in TOML — no Rust keyword collision.
                // A bare expression is wrapped in the documented single-tag
                // form; a source that already carries tags is emitted as-is, so
                // a config override round-trips instead of being double-wrapped.
                let value = if crate::eval::classify(expr) == crate::eval::Form::Expression {
                    format!("{{{{ {expr} }}}}")
                } else {
                    expr.to_string()
                };
                let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
                out.push_str(&format!("virtual.{field} = \"{escaped}\"\n"));
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
        // `stack` holds exactly the fields currently being evaluated, so its
        // length is this recursion's depth. The cycle check above only stops a
        // field repeating; a chain of distinct fields recurses once per link
        // and is bounded here instead. See `crate::eval::MAX_VIRTUAL_DEPTH`.
        if stack.len() >= crate::eval::MAX_VIRTUAL_DEPTH {
            return Err(crate::eval::too_deep());
        }
        let expr = self
            .expression(provider, field)
            .ok_or_else(|| format!("{provider}.{field} is not a virtual field"))?;
        stack.insert(key.clone());
        let result = self.evaluate_expression(expr, ctx, stack);
        stack.remove(&key);
        // Name the field the failure came from. Without this a bad expression in
        // one config virtual field surfaces as a bare "expression compile error:
        // ..." with nothing pointing at which field to go and fix. The cycle
        // error above already names the field, and is returned before this
        // point, so it is never prefixed twice.
        result.map_err(|e| format!("{provider}.{field}: {e}"))
    }

    /// Evaluate an arbitrary value expression against the given context.
    ///
    /// Accepts all three forms (canon `field_resolution.md` invariant 14) —
    /// bare, a single `{{ }}` tag, or a template — via [`crate::eval`]. Refs
    /// are discovered per form, then bound:
    ///
    /// - `Env(X)` refs → `ctx.env_vars.get(X)` (empty string on miss).
    /// - `CacheField(P, F)` refs → `ctx.daemon_data["P.F"]` (raw cached value).
    /// - `CacheProvider(P)` refs → `ctx.daemon_data["P"]` (whole provider object).
    /// - `Resolved(P, F)` refs → if virtual, recurse `self.evaluate(P, F, ...)`
    ///   with cycle detection; otherwise `ctx.daemon_data["P.F"]`.
    ///
    /// Crate-private: [`crate::eval::evaluate`] is the public entry point for
    /// evaluating a value expression, and this is only the thread-the-stack
    /// variant [`Self::evaluate`] needs. Demoted in the same release as
    /// [`discover_expression_refs`], for the same reason — two public spellings
    /// of one operation, where only one of them handles all three forms
    /// correctly by name.
    pub(crate) fn evaluate_expression(
        &self,
        expr: &str,
        ctx: &EvalContext<'_>,
        stack: &mut HashSet<(String, String)>,
    ) -> Result<JsonValue, String> {
        crate::eval::evaluate_with_stack(expr, self, ctx, stack)
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
///
/// **A miss binds nothing**, in every arm: not the value, and not the object
/// that would have enclosed it. The ref is then *undefined* rather than `none`,
/// which is what makes `{{ p.f | default("FB") }}` fall back on a cache miss —
/// MiniJinja's `default` filter replaces an undefined value, not a null one, so
/// binding a miss to `JsonValue::Null` would silently swallow every `default`
/// in the wild. Nothing needs a placeholder empty map: `{{ p.f.sub }}` chains
/// through undefined under `build_expression_env`'s
/// `UndefinedBehavior::Chainable`, and a sibling hit on `p.g` creates `p`
/// through its own `entry()`. Pre-creating one would instead make
/// `{{ p | default("FB") }}` render `{}` whenever a field ref for the same
/// provider happened to co-occur.
///
/// Where a `CacheProvider(P)` object and a `CacheField(P, F)` disagree about
/// `F`, the whole object wins — it is the authoritative snapshot. That holds
/// **whichever ref is bound first**: the provider arm overrides on merge, and
/// the field arm only fills a key the object did not supply. Should the whole
/// provider value not be an object at all, the field arm leaves it untouched —
/// a field of a scalar is not navigable either way. So the result does not
/// depend on `refs` order.
pub(crate) fn build_context_json(
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
                // A miss binds nothing — not the field, and not the `cache.P` map
                // that would enclose it — so both stay undefined and `default`
                // fires on either.
                let Some(raw_val) = ctx.daemon_data.get(&format!("{provider}.{field}")).cloned()
                else {
                    continue;
                };
                let cache_entry = top
                    .entry("cache".to_string())
                    .or_insert_with(|| JsonValue::Object(serde_json::Map::new()));
                if let JsonValue::Object(cache_map) = cache_entry {
                    // If a CacheProvider binding already inserted a whole object for
                    // this provider, insert just the individual field key into it
                    // rather than replacing the whole map — and never over a key the
                    // whole object already supplied: that object is the authoritative
                    // snapshot, so it wins on collision whichever ref is bound first.
                    let provider_entry = cache_map
                        .entry(provider.clone())
                        .or_insert_with(|| JsonValue::Object(serde_json::Map::new()));
                    if let JsonValue::Object(pmap) = provider_entry {
                        pmap.entry(field.clone()).or_insert(raw_val);
                    }
                    // If provider_entry is not an Object (a non-object whole-provider
                    // value), leave it — a field of a scalar is not navigable anyway.
                }
            }

            Ref::CacheProvider(provider) => {
                // A miss binds nothing, so `{{ cache.P | default(...) }}` falls back.
                let Some(whole_obj) = ctx.daemon_data.get(provider.as_str()).cloned() else {
                    continue;
                };
                let cache_entry = top
                    .entry("cache".to_string())
                    .or_insert_with(|| JsonValue::Object(serde_json::Map::new()));
                if let JsonValue::Object(cache_map) = cache_entry {
                    // Merge whole-object keys into any existing partial map so that
                    // CacheField bindings that were already inserted (e.g. `cache.P.F`)
                    // are not lost.  Whole-object values win on key collision (the
                    // whole object is the authoritative snapshot); keys already in the
                    // partial map that are absent from the whole object survive.
                    if let Some(JsonValue::Object(existing)) = cache_map.get_mut(provider) {
                        if let JsonValue::Object(whole_map) = whole_obj {
                            for (k, v) in whole_map {
                                existing.insert(k, v);
                            }
                        }
                        // If whole_obj is not an object, leave existing as-is.
                    } else {
                        cache_map.insert(provider.clone(), whole_obj);
                    }
                }
            }

            Ref::Resolved(provider, field) => {
                // A virtual field always resolves to a value (an all-empty cascade
                // is `""`); a plain daemon ref that missed binds nothing — neither
                // the field nor the `P` map that would enclose it.
                let resolved_val = if vf.is_virtual(provider, field) {
                    Some(vf.evaluate(provider, field, ctx, stack)?)
                } else {
                    ctx.daemon_data.get(&format!("{provider}.{field}")).cloned()
                };
                let Some(v) = resolved_val else { continue };
                let provider_entry = top
                    .entry(provider.clone())
                    .or_insert_with(|| JsonValue::Object(serde_json::Map::new()));
                if let JsonValue::Object(pmap) = provider_entry {
                    pmap.insert(field.clone(), v);
                }
            }
        }
    }

    Ok(JsonValue::Object(top))
}

// ── Ref discovery ─────────────────────────────────────────────────────────────

/// Classify one dotted name from minijinja's undeclared-variable analysis.
///
/// - `env.X` → `Ref::Env(X)`
/// - `cache.P.F` → `Ref::CacheField(P, F)`
/// - `cache.P` → `Ref::CacheProvider(P)`
/// - `cwd` → `None` (path-expression variable, reserved for a later task)
/// - `P.F` (P ∉ {env, cache, cwd}) → `Ref::Resolved(P, F)`
/// - bare single name → `None`
///
/// Segments beyond the second (third for `cache.*`) are MiniJinja attribute
/// navigation into the fetched value — NOT part of the ref key itself.
///
/// Shared by [`discover_expression_refs`] and the template discovery path in
/// [`crate::eval`], so expression and template forms classify identically.
pub(crate) fn classify_dotted(name: &str) -> Option<Ref> {
    let mut segments = name.split('.');
    let first = segments.next()?;
    // Bare name — no ref (includes "cwd").
    let second = segments.next()?;
    let third = segments.next();

    match first {
        "env" => Some(Ref::Env(second.to_string())),
        "cache" => Some(match third {
            Some(field) => Ref::CacheField(second.to_string(), field.to_string()),
            None => Ref::CacheProvider(second.to_string()),
        }),
        // Path-expression variable — reserved for a later task; ignore here.
        "cwd" => None,
        _ => Some(Ref::Resolved(first.to_string(), second.to_string())),
    }
}

/// Classify a batch of undeclared-variable names into a deduplicated ref list.
///
/// Shared by [`discover_expression_refs`] and the template discovery path in
/// `crate::eval`, so the expression and template forms of one value expression
/// always yield the same refs.
///
/// VERIFIED against vendor/minijinja 2.19.0 (compiler/meta.rs:141-157): with
/// nested = true, an attribute chain `a.b` is recorded as the dotted string
/// "a.b" (e.g. `env.PYENV_VERSION or mise.python` → {"env.PYENV_VERSION",
/// "mise.python"}). So `classify_dotted` splits each entry on '.' — no
/// byte-scanning needed.
pub(crate) fn refs_from_names(names: impl IntoIterator<Item = String>) -> Vec<Ref> {
    let mut refs: Vec<Ref> = Vec::new();
    let mut seen: HashSet<Ref> = HashSet::new();
    for name in names {
        if let Some(r) = classify_dotted(&name)
            && seen.insert(r.clone())
        {
            refs.push(r);
        }
    }
    refs
}

/// Discover all refs in an expression using minijinja's
/// `Expression::undeclared_variables(true)` (nested = true), classified by
/// `classify_dotted`.
///
/// Returns a deduplicated list of refs, sorted for reproducibility — see
/// [`crate::eval::discover_refs`], which states the rule for every discovery
/// path in this crate.
///
/// Takes an expression, not a value expression: a source written with `{{ }}`
/// fails to compile here and yields nothing. [`crate::eval::discover_refs`] is
/// the entry point that handles all three forms, and is what callers outside
/// this module use.
pub(crate) fn discover_expression_refs(expr: &str) -> Vec<Ref> {
    let env = build_expression_env();
    let Ok(compiled) = env.compile_expression(expr) else {
        return vec![];
    };
    let mut refs = refs_from_names(compiled.undeclared_variables(true));
    refs.sort();
    refs
}

// ── Minijinja environment for expressions ─────────────────────────────────────

/// Build a minijinja `Environment` suitable for `compile_expression`.
///
/// Registers the same filters as `build_env()` (truncate, basename) and sets
/// chainable undefined behavior so missing refs are falsy, not errors.
///
/// `Chainable` rather than `Lenient` because canon `field_resolution.md` says a
/// missing ref is falsy, and that has to hold at any depth: `Lenient` renders
/// an undefined value as empty but *errors* on attribute access into one, so
/// `{{ p.f.sub }}` blew up whenever `p.f` missed while `{{ p.f }}` quietly
/// rendered nothing. Chaining through undefined yields undefined instead.
/// Cascades are unaffected — undefined is falsy under both — and `default`
/// still fires, since `build_context_json` leaves a missed key absent.
pub(crate) fn build_expression_env<'a>() -> Environment<'a> {
    use crate::filters::build_env;
    let mut env = build_env();
    env.set_undefined_behavior(UndefinedBehavior::Chainable);
    env
}

// ── Value conversion ──────────────────────────────────────────────────────────

/// Convert a minijinja `Value` to a `serde_json::Value`.
///
/// Preserves types: bool → bool, integer → number, string → string, map →
/// object, sequence → array. UNDEFINED / None → empty string (all-falsy
/// result, per canon `field_resolution.md` invariant 9: an all-empty cascade
/// resolves to `""`).
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
    // Structured values keep their shape: an interior node is an object and a
    // sequence an array, rather than MiniJinja's `{"a": 1}` debug rendering —
    // canon `field_resolution.md` invariants 8 and 12. serde round-trips the
    // whole tree (a nested `none` becomes JSON null); anything it cannot
    // represent falls through to the string form below.
    if matches!(
        v.kind(),
        minijinja::value::ValueKind::Map
            | minijinja::value::ValueKind::Seq
            | minijinja::value::ValueKind::Iterable
    ) && let Ok(json) = serde_json::to_value(&v)
    {
        return json;
    }
    // String fallback — also handles UNDEFINED that slipped through.
    let s = v.to_string();
    JsonValue::String(s)
}
