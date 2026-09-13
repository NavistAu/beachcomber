//! Client-side virtual field evaluator.
//!
//! A virtual field's value expression is written as a single tag
//! (`{{ env.A or cache.x.y }}`) or as a template
//! (`{{ git.branch }}{% if git.dirty %}*{% endif %}`). The former evaluates
//! to a typed `serde_json::Value`; a template is string-valued (a source with
//! no tags at all is literal text). [`crate::eval`] owns that classification
//! and the evaluation itself; this module owns the registry, the reference
//! taxonomy, and the context the refs bind to. Ref discovery uses minijinja's
//! `undeclared_variables(true)` meta-analysis to enumerate all
//! `provider.field` and `env.*` refs — no byte-level scanning.
//!
//! Built-in default virtual fields are compiled into `libbeachcomber`; no
//! config file is required. Config may override or extend them. Any binary
//! that links this crate gets them — the `comb` CLI today, and a statusline
//! renderer plus C ABI / language SDK bindings in later phases.
//!
//! RUST NOTE: `virtual` is a reserved keyword in Rust. This module is named
//! `virtual_fields`. The TOML key "virtual" is read as a string literal
//! `"virtual"` in Rust code — e.g. `table.get("virtual")` — which is legal.

use minijinja::value::{Kwargs, Rest, Value as MjValue, ValueKind};
use minijinja::{Environment, Error, ErrorKind, UndefinedBehavior};
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
        "{{ env.TF_WORKSPACE or cache.terraform.workspace }}",
    ),
    // python
    (
        "python",
        "version",
        "{{ env.PYENV_VERSION or env.MISE_PYTHON_VERSION or cache.mise.python or cache.asdf.python or cache.python.venv_version }}",
    ),
    (
        "python",
        "venv_name",
        r#"{{ cache.python.local_venv_name or (env.VIRTUAL_ENV | basename) }}"#,
    ),
    // conda (daemon provider removed; virtual from env only)
    ("conda", "env", "{{ env.CONDA_DEFAULT_ENV }}"),
    // op (daemon provider removed; virtual from env only)
    // Security: expression returns a bool, NOT the token string.
    (
        "op",
        "signed_in",
        r#"{{ env.OP_SERVICE_ACCOUNT_TOKEN != "" }}"#,
    ),
    // aws
    (
        "aws",
        "profile",
        r#"{{ env.AWS_PROFILE or env.AWS_VAULT or env.AWS_DEFAULT_PROFILE or "default" }}"#,
    ),
    (
        "aws",
        "region",
        r#"{{ env.AWS_REGION or env.AWS_DEFAULT_REGION or cache.aws_profiles[ env.AWS_PROFILE or env.AWS_VAULT or env.AWS_DEFAULT_PROFILE or "default" ].region }}"#,
    ),
    (
        "aws",
        "expiration",
        "{{ env.AWS_CREDENTIAL_EXPIRATION or env.AWS_SESSION_EXPIRATION }}",
    ),
    // gcloud
    (
        "gcloud",
        "project",
        "{{ env.CLOUDSDK_CORE_PROJECT or cache.gcloud_configs[ env.CLOUDSDK_ACTIVE_CONFIG_NAME or cache.gcloud_configs.active_config ].project }}",
    ),
    (
        "gcloud",
        "account",
        "{{ cache.gcloud_configs[ env.CLOUDSDK_ACTIVE_CONFIG_NAME or cache.gcloud_configs.active_config ].account }}",
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
    /// Under the resolver's deep classifier (`classify_dotted_deep`) the
    /// second component is the whole remaining address (`"widgets.b"`), not
    /// one segment.
    Resolved(String, String),
}

/// The evaluation context: resolved env vars + pre-fetched daemon data.
///
/// All fields are private, so these constructors are the only way to build
/// one: [`EvalContext::new`], and [`EvalContext::with_exec`] to additionally
/// opt into the `exec()` jinja function.
pub struct EvalContext<'a> {
    /// The calling shell's environment variables.
    env_vars: &'a HashMap<String, String>,
    /// Pre-fetched daemon values:
    /// - key = "provider.field" for individual fields
    /// - key = "provider" for whole-provider objects (CacheProvider refs)
    daemon_data: &'a HashMap<String, JsonValue>,
    /// Whether the `exec()` jinja function is available during evaluation.
    allow_exec: bool,
}

impl<'a> EvalContext<'a> {
    /// Build a context from the calling shell's environment variables and
    /// pre-fetched daemon data, with `exec()` unavailable.
    pub fn new(
        env_vars: &'a HashMap<String, String>,
        daemon_data: &'a HashMap<String, JsonValue>,
    ) -> Self {
        Self {
            env_vars,
            daemon_data,
            allow_exec: false,
        }
    }

    /// Opt into the `exec()` jinja function. Enabling this lets an evaluated
    /// expression spawn processes, so only trusted expression sources should
    /// get it.
    pub fn with_exec(mut self) -> Self {
        self.allow_exec = true;
        self
    }

    /// Whether the `exec()` jinja function is available during evaluation.
    pub(crate) fn allow_exec(&self) -> bool {
        self.allow_exec
    }
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
    /// Every expression is emitted exactly as stored — `BUILTIN_DEFAULTS` are
    /// already written in the documented single-tag form (canon
    /// `field_resolution.md` invariant 14), and a config override is written
    /// back in whatever form it was read in. Rewriting a tagless override
    /// would silently turn its literal text into an expression, so nothing
    /// here wraps a source that lacks tags.
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
                // TOML dotted-key form: virtual.<field> = "<expression, as stored>"
                // "virtual" is read as a string key in TOML — no Rust keyword collision.
                let escaped = expr.replace('\\', "\\\\").replace('"', "\\\"");
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
    /// Accepts both forms (canon `field_resolution.md` invariant 14) — a
    /// single `{{ }}` tag, or a template — via [`crate::eval`]. Refs are
    /// discovered per form, then bound:
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
    /// of one operation, where only one of them handles both forms
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
/// Fields that fail to evaluate (e.g., cycle errors, an exec-disabled or
/// exec-failed field under an exec-off `ctx`) are silently omitted. Callers
/// may merge additional daemon-backed fields on top.
///
/// `ctx` is caller-supplied rather than built internally, so a caller that
/// wants `exec()` available across the whole namespace passes one built with
/// [`EvalContext::with_exec`]. The CLI's bare-provider `comb get` namespace
/// path does exactly that (the CLI user already has a shell); the FFI/SDK
/// surface stays exec-off.
pub fn evaluate_namespace(provider: &str, vf: &VirtualFields, ctx: &EvalContext<'_>) -> JsonValue {
    let mut obj = serde_json::Map::new();
    for field in vf.fields_for(provider) {
        let mut stack = HashSet::new();
        match vf.evaluate(provider, &field, ctx, &mut stack) {
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

/// [`classify_dotted`] for the one-tree resolver ([`crate::resolve`]), where
/// every node is addressable: a `P.rest…` name is `Resolved(P, "rest…")`
/// with every segment kept, so `scanline.widgets.b` names that leaf and
/// never the group above it. `env.*`, `cache.*`, `cwd` and a bare name
/// classify exactly as [`classify_dotted`] does.
pub(crate) fn classify_dotted_deep(name: &str) -> Option<Ref> {
    let (first, rest) = name.split_once('.')?;
    match first {
        "env" | "cache" | "cwd" => classify_dotted(name),
        _ => Some(Ref::Resolved(first.to_string(), rest.to_string())),
    }
}

/// Classify a batch of undeclared-variable names into a deduplicated ref list.
///
/// Shared by [`discover_expression_refs`] and the template discovery path in
/// `crate::eval`, so the expression and template forms of one value expression
/// always yield the same refs. `classify` is [`classify_dotted`] or
/// [`classify_dotted_deep`]; everything but the classification is shared.
///
/// VERIFIED against vendor/minijinja 2.19.0 (compiler/meta.rs:141-157): with
/// nested = true, an attribute chain `a.b.c` is recorded as the dotted string
/// "a.b.c" (e.g. `env.PYENV_VERSION or mise.python` → {"env.PYENV_VERSION",
/// "mise.python"}). So the classifier splits each entry on '.' — no
/// byte-scanning needed.
pub(crate) fn refs_from_names(
    names: impl IntoIterator<Item = String>,
    classify: fn(&str) -> Option<Ref>,
) -> Vec<Ref> {
    let mut refs: Vec<Ref> = Vec::new();
    let mut seen: HashSet<Ref> = HashSet::new();
    for name in names {
        if let Some(r) = classify(&name)
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
/// the entry point that handles both forms, and is what callers outside
/// this module use.
pub(crate) fn discover_expression_refs(expr: &str) -> Vec<Ref> {
    discover_expression_refs_with(expr, classify_dotted)
}

/// [`discover_expression_refs`] with the classifier chosen by the caller —
/// [`classify_dotted_deep`] for [`crate::resolve`].
pub(crate) fn discover_expression_refs_with(
    expr: &str,
    classify: fn(&str) -> Option<Ref>,
) -> Vec<Ref> {
    // Ref discovery never evaluates anything, so `allow_exec` doesn't
    // matter here — `false` is an arbitrary, harmless choice. (This has
    // nothing to do with `exec` showing up as an "undeclared variable":
    // minijinja's meta-analysis never reports a call's callee name at all,
    // registered or not.)
    let env = build_expression_env(false);
    let Ok(compiled) = env.compile_expression(expr) else {
        return vec![];
    };
    let mut refs = refs_from_names(compiled.undeclared_variables(true), classify);
    refs.sort();
    refs
}

// ── Minijinja environment for expressions ─────────────────────────────────────

/// Build a minijinja `Environment` suitable for `compile_expression`.
///
/// Registers the same filters as `build_env()`, sets
/// chainable undefined behavior so missing refs are falsy, not errors, and
/// registers the `exec()` jinja function: the real closure ([`exec_fn`])
/// when `allow_exec` is true, a closure that always errors ([`exec_disabled_fn`])
/// otherwise. Both modes register *some* function named `exec` so a
/// disabled call fails with the on-topic "exec() is disabled in this
/// context" rather than minijinja's generic "unknown function: exec" —
/// indistinguishable, to the person reading it, from a typo'd function
/// name.
///
/// `Chainable` rather than `Lenient` because canon `field_resolution.md` says a
/// missing ref is falsy, and that has to hold at any depth: `Lenient` renders
/// an undefined value as empty but *errors* on attribute access into one, so
/// `{{ p.f.sub }}` blew up whenever `p.f` missed while `{{ p.f }}` quietly
/// rendered nothing. Chaining through undefined yields undefined instead.
/// Cascades are unaffected — undefined is falsy under both — and `default`
/// still fires, since `build_context_json` leaves a missed key absent.
pub(crate) fn build_expression_env<'a>(allow_exec: bool) -> Environment<'a> {
    use crate::filters::build_env;
    let mut env = build_env();
    env.set_undefined_behavior(UndefinedBehavior::Chainable);
    if allow_exec {
        env.add_function("exec", exec_fn);
    } else {
        env.add_function("exec", exec_disabled_fn);
    }
    env
}

// ── exec() ────────────────────────────────────────────────────────────────

/// The `exec()` stub registered when `allow_exec` is false: same name and
/// calling convention as [`exec_fn`], so a template that calls `exec(...)`
/// fails at the call site with a clear message rather than at compile time
/// with an "unknown function" error.
fn exec_disabled_fn(_args: Rest<MjValue>, _kwargs: Kwargs) -> Result<MjValue, Error> {
    Err(Error::new(
        ErrorKind::InvalidOperation,
        "exec() is disabled in this context",
    ))
}

/// The real `exec()` jinja function, registered only when `allow_exec` is
/// true: `exec(cmd, arg1, arg2, ..., timeout_ms=10000, stdin=none)`, backed
/// by the spawn engine in [`crate::exec`].
///
/// `cmd` (the first positional) must be a string. Every remaining positional
/// is coerced to a string argv entry — see [`coerce_exec_argv`]. `timeout_ms`
/// (default 10000) must convert to an integer; `stdin` (default none) must
/// be a string — `| tojson` first for structured data, symmetric with argv
/// coercion rejecting a sequence or map. Any other keyword argument is an
/// error.
fn exec_fn(args: Rest<MjValue>, kwargs: Kwargs) -> Result<MjValue, Error> {
    let cmd = args.first().ok_or_else(|| {
        Error::new(
            ErrorKind::MissingArgument,
            "exec() requires at least a command argument",
        )
    })?;
    let cmd = cmd.as_str().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidOperation,
            "exec(): argument 1 (the command) must be a string",
        )
    })?;

    let mut argv = Vec::with_capacity(args.len().saturating_sub(1));
    for (i, v) in args.iter().enumerate().skip(1) {
        argv.push(coerce_exec_argv(v, i + 1)?);
    }

    // A bare `?` here would surface a conversion failure (e.g.
    // `timeout_ms="x"`) as minijinja's generic "cannot convert string to
    // u64", with nothing naming `exec()` or which keyword caused it.
    let timeout_ms = kwargs
        .get::<Option<u64>>("timeout_ms")
        .map_err(|e| {
            Error::new(
                ErrorKind::InvalidOperation,
                format!(
                    "exec(): timeout_ms= {}",
                    e.detail().unwrap_or("invalid value")
                ),
            )
        })?
        .unwrap_or(10_000);
    // Fetched as a raw `Value` (not `Option<String>`) so a non-string —
    // e.g. `stdin=true` or `stdin={"a": 1}` — is rejected instead of
    // silently stringifying through minijinja's `Display` (`"True"`, a
    // jinja-repr map), which would be asymmetric with argv coercion
    // rejecting the same shapes outright.
    let stdin = match kwargs.get::<Option<MjValue>>("stdin")? {
        None => None,
        Some(v) => match v.as_str() {
            Some(s) => Some(s.to_string()),
            None => {
                return Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "exec(): stdin= must be a string; use | tojson to pass structured data",
                ));
            }
        },
    };
    kwargs.assert_all_used()?;

    let req = crate::exec::ExecRequest {
        cmd,
        args: &argv,
        stdin: stdin.as_deref(),
        timeout_ms,
    };
    crate::exec::run(&req)
        .map(MjValue::from)
        .map_err(|e| Error::new(ErrorKind::InvalidOperation, e))
}

/// Coerce one `exec()` positional argument (beyond `cmd`) into an argv
/// string. `position` is the argument's 1-based position in the call, used
/// to name it in an error.
///
/// A string (or valid-UTF8 bytes) passes through as-is; a bool renders as
/// the literal `"true"`/`"false"` — not minijinja's `Display`, which prints
/// `True`/`False`; an integer or float renders in minijinja's canonical
/// string form; an undefined or none value is an error (most commonly a
/// missed daemon ref reaching `exec()` un-guarded); a sequence or map is an
/// error naming the argument and directing the caller to `stdin=` instead —
/// argv entries are scalars, not structured data.
fn coerce_exec_argv(v: &MjValue, position: usize) -> Result<String, Error> {
    if let Some(s) = v.as_str() {
        return Ok(s.to_string());
    }
    match v.kind() {
        ValueKind::Bool => Ok(if v.is_true() { "true" } else { "false" }.to_string()),
        ValueKind::Number => Ok(v.to_string()),
        ValueKind::Undefined | ValueKind::None => Err(Error::new(
            ErrorKind::InvalidOperation,
            format!("exec(): argument {position} is undefined or none"),
        )),
        _ => Err(Error::new(
            ErrorKind::InvalidOperation,
            format!(
                "exec(): argument {position} is not a scalar; pass structured data via stdin= instead"
            ),
        )),
    }
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

// ── exec() tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::evaluate;

    fn empty_maps() -> (HashMap<String, String>, HashMap<String, JsonValue>) {
        (HashMap::new(), HashMap::new())
    }

    // Canon `field_resolution.md` invariant 14: the single-tag and template
    // forms are the same one expression syntax and must agree on whether
    // exec() is available. This test and the pair immediately below pin the
    // single-tag form (this one with multiple scalar argument types); the
    // template-form pair near the end of this module
    // (`exec_enabled_in_template_form_when_with_exec` /
    // `exec_disabled_in_template_form_without_with_exec`) pins the other.
    #[test]
    fn exec_enabled_env_runs_command_with_scalar_args() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data).with_exec();

        let result = evaluate(r#"{{ exec("/bin/echo", "hi", 42) }}"#, &vf, &ctx).unwrap();
        assert_eq!(result, JsonValue::String("hi 42".to_string()));
    }

    #[test]
    fn exec_argv_separation_is_pinned() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data).with_exec();

        // `sh -c script arg0 arg1 arg2` sets $0 = arg0, so "_" fills $0 and
        // $1/$2 are "x"/"y" — $# must be 2, not 3, pinning that `cmd` +
        // remaining positionals map onto argv exactly (no accidental $0
        // duplication or off-by-one).
        let result = evaluate(
            r#"{{ exec("sh", "-c", "echo $#", "_", "x", "y") }}"#,
            &vf,
            &ctx,
        )
        .unwrap();
        assert_eq!(result, JsonValue::String("2".to_string()));
    }

    #[test]
    fn exec_disabled_by_default() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data);

        let err = evaluate(r#"{{ exec("/bin/echo", "hi") }}"#, &vf, &ctx).unwrap_err();
        // minijinja's Display prefixes the error kind, and `eval` adds its
        // own "expression eval error: " prefix — assert the substring, not
        // the whole message.
        assert!(
            err.contains("exec() is disabled in this context"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn exec_enabled_in_single_tag_form() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data).with_exec();

        // Exactly one `{{ }}` tag spanning the whole source: the typed path —
        // natural-type result, not a string wrapping a rendered template.
        let result = evaluate(r#"{{ exec("/bin/echo", "hi") }}"#, &vf, &ctx).unwrap();
        assert_eq!(result, JsonValue::String("hi".to_string()));
    }

    #[test]
    fn exec_disabled_in_single_tag_form() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data);

        let err = evaluate(r#"{{ exec("/bin/echo", "hi") }}"#, &vf, &ctx).unwrap_err();
        assert!(
            err.contains("exec() is disabled in this context"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn exec_undefined_arg_names_its_1_based_position() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data).with_exec();

        // `cache.missing.value` is a daemon ref that never got fetched (not
        // in `data`), so it binds to nothing and evaluates as chainable
        // undefined — landing as exec()'s 2nd positional (1-based).
        let err =
            evaluate(r#"{{ exec("/bin/echo", cache.missing.value) }}"#, &vf, &ctx).unwrap_err();
        assert!(err.contains("argument 2"), "unexpected error: {err}");
    }

    #[test]
    fn exec_stdin_kwarg_reaches_the_child() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data).with_exec();

        let result = evaluate(r#"{{ exec("cat", stdin="round trip") }}"#, &vf, &ctx).unwrap();
        assert_eq!(result, JsonValue::String("round trip".to_string()));
    }

    #[test]
    fn exec_unknown_kwarg_errors() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data).with_exec();

        let err = evaluate(r#"{{ exec("/bin/echo", "hi", bogus=1) }}"#, &vf, &ctx).unwrap_err();
        assert!(err.contains("bogus"), "unexpected error: {err}");
    }

    #[test]
    fn exec_bool_arg_coerces_to_lowercase_literal() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data).with_exec();

        // minijinja's own `Display` for a bool prints "True"/"False"
        // (Python-style); exec() must emit the canonical "true"/"false".
        let result = evaluate(r#"{{ exec("/bin/echo", true, false) }}"#, &vf, &ctx).unwrap();
        assert_eq!(result, JsonValue::String("true false".to_string()));
    }

    #[test]
    fn exec_stdin_rejects_non_string_value() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data).with_exec();

        // A bool (or a map/seq) must not silently stringify through
        // minijinja's `Display` — symmetric with argv coercion, which
        // rejects the same shapes outright.
        let err = evaluate(r#"{{ exec("cat", stdin=true) }}"#, &vf, &ctx).unwrap_err();
        assert!(err.contains("stdin"), "unexpected error: {err}");
        assert!(err.contains("tojson"), "unexpected error: {err}");
    }

    #[test]
    fn exec_timeout_ms_conversion_error_names_the_kwarg() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data).with_exec();

        let err = evaluate(
            r#"{{ exec("/bin/echo", "hi", timeout_ms="x") }}"#,
            &vf,
            &ctx,
        )
        .unwrap_err();
        assert!(err.contains("timeout_ms"), "unexpected error: {err}");
    }

    #[test]
    fn exec_seq_arg_is_rejected_with_its_position() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data).with_exec();

        let err = evaluate(r#"{{ exec("/bin/echo", "hi", [1, 2]) }}"#, &vf, &ctx).unwrap_err();
        assert!(err.contains("argument 3"), "unexpected error: {err}");
        assert!(err.contains("stdin="), "unexpected error: {err}");
    }

    #[test]
    fn exec_non_string_cmd_errors() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data).with_exec();

        let err = evaluate(r#"{{ exec(42) }}"#, &vf, &ctx).unwrap_err();
        assert!(err.contains("argument 1"), "unexpected error: {err}");
    }

    #[test]
    fn exec_runtime_failure_is_an_eval_error_not_a_compile_error() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data).with_exec();

        // `libbeachcomber-ffi::eval_error_kind` matches the substring
        // "compile error" to classify a failure as the caller's own source
        // being malformed; a real command failure must never read as one.
        let err = evaluate(r#"{{ exec("sh", "-c", "exit 3") }}"#, &vf, &ctx).unwrap_err();
        assert!(
            err.starts_with("expression eval error: "),
            "unexpected error: {err}"
        );
        assert!(
            !err.contains("compile error"),
            "must not read as a compile error: {err}"
        );
    }

    #[test]
    fn exec_enabled_in_template_form_when_with_exec() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data).with_exec();

        let result = evaluate(r#"prefix {{ exec("/bin/echo", "hi") }} suffix"#, &vf, &ctx).unwrap();
        assert_eq!(result, JsonValue::String("prefix hi suffix".to_string()));
    }

    #[test]
    fn exec_disabled_in_template_form_without_with_exec() {
        let vf = VirtualFields::defaults_only();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data);

        let err =
            evaluate(r#"prefix {{ exec("/bin/echo", "hi") }} suffix"#, &vf, &ctx).unwrap_err();
        assert!(
            err.contains("exec() is disabled in this context"),
            "unexpected error: {err}"
        );
    }

    // ── evaluate_namespace + exec ────────────────────────────────────────────

    /// A provider with one exec-backed virtual field and one plain sibling,
    /// shared by the three `evaluate_namespace`/exec tests below.
    fn exec_namespace_fields() -> VirtualFields {
        VirtualFields::with_config_overrides([
            (
                ("exectest".to_string(), "greeting".to_string()),
                r#"{{ exec("/bin/echo", "hi") }}"#.to_string(),
            ),
            (
                ("exectest".to_string(), "plain".to_string()),
                r#"{{ "ok" }}"#.to_string(),
            ),
        ])
    }

    #[test]
    fn namespace_with_exec_enabled_context_includes_exec_backed_field() {
        let vf = exec_namespace_fields();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data).with_exec();

        let result = evaluate_namespace("exectest", &vf, &ctx);
        let obj = result.as_object().expect("must return object");
        assert_eq!(
            obj.get("greeting"),
            Some(&JsonValue::String("hi".to_string())),
            "exec-backed field must be present when exec is enabled: {obj:?}"
        );
        assert_eq!(
            obj.get("plain"),
            Some(&JsonValue::String("ok".to_string())),
            "plain sibling field must still be present: {obj:?}"
        );
    }

    #[test]
    fn namespace_with_exec_off_context_silently_omits_exec_backed_field() {
        let vf = exec_namespace_fields();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data);

        let result = evaluate_namespace("exectest", &vf, &ctx);
        let obj = result.as_object().expect("must return object");
        assert!(
            !obj.contains_key("greeting"),
            "exec-backed field must be silently omitted (not an error key) when exec is off: {obj:?}"
        );
        assert_eq!(
            obj.get("plain"),
            Some(&JsonValue::String("ok".to_string())),
            "plain sibling field must still be present: {obj:?}"
        );
    }

    #[test]
    fn single_field_evaluate_of_exec_backed_field_errors_loudly_when_exec_off() {
        let vf = exec_namespace_fields();
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data);

        // Same field `evaluate_namespace` silently drops above — evaluated
        // directly (as `comb get exectest.greeting` would), it must surface
        // the disabled-exec error rather than being swallowed.
        let err = vf
            .evaluate("exectest", "greeting", &ctx, &mut HashSet::new())
            .unwrap_err();
        assert!(
            err.contains("exec() is disabled in this context"),
            "unexpected error: {err}"
        );
    }

    // ── exec() args referencing another virtual field ────────────────────────

    #[test]
    fn exec_arg_referencing_another_virtual_field_respects_chain_depth() {
        // `chain.a`'s exec() argument references `chain.b`, a distinct virtual
        // field — a two-level chain. `build_context_json` must resolve `chain.b`
        // (threading the cycle-detection stack) before `exec()` ever runs, and
        // the recursion must land within MAX_VIRTUAL_DEPTH same as any other
        // virtual-field chain (`crate::eval`'s `deeply_nested_virtual_fields_*`
        // tests pin the limit itself; this pins that exec() args are not a
        // special case that bypasses it).
        let vf = VirtualFields::with_config_overrides([
            (
                ("chain".to_string(), "b".to_string()),
                r#"{{ "bee" }}"#.to_string(),
            ),
            (
                ("chain".to_string(), "a".to_string()),
                r#"{{ exec("/bin/echo", chain.b) }}"#.to_string(),
            ),
        ]);
        let (env, data) = empty_maps();
        let ctx = EvalContext::new(&env, &data).with_exec();

        let result = vf
            .evaluate("chain", "a", &ctx, &mut HashSet::new())
            .unwrap();
        assert_eq!(result, JsonValue::String("bee".to_string()));
    }
}
