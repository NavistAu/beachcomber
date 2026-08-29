//! Evaluation of all three value-expression forms.
//!
//! Pins canon `field_resolution.md` invariant 14 on the evaluating side: a
//! value expression written as exactly one `{{ expr }}` evaluates to the
//! expression's natural type; one written with literal text or more than one
//! tag evaluates to a string; a bare expression is equivalent to the
//! single-tag form.

use libbeachcomber::eval::{
    Form, classify, daemon_refs, discover_refs, evaluate, render_template, single_tag_expression,
};
use libbeachcomber::virtual_fields::{EvalContext, Ref, VirtualFields};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

// ── Fixtures ──────────────────────────────────────────────────────────────────

fn vfields(entries: &[(&str, &str, &str)]) -> VirtualFields {
    VirtualFields::with_config_overrides(
        entries
            .iter()
            .map(|(p, f, e)| ((p.to_string(), f.to_string()), e.to_string())),
    )
}

fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn data_of(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

// ── Single-tag form keeps the expression's natural type ───────────────────────

#[test]
fn single_tag_keeps_bool() {
    let vf = VirtualFields::defaults_only();
    let env = env_of(&[("T", "yes")]);
    let data = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };

    // One tag spanning the whole source: the comparison's bool survives.
    assert_eq!(
        evaluate(r#"{{ env.T != "" }}"#, &vf, &ctx).unwrap(),
        Value::Bool(true)
    );
    // The bare form is equivalent.
    assert_eq!(
        evaluate(r#"env.T != """#, &vf, &ctx).unwrap(),
        Value::Bool(true)
    );
    // Surrounding whitespace is not literal text (canon: "whitespace around a
    // single tag is not literal text").
    assert_eq!(
        evaluate("  {{ env.T != \"\" }}\n", &vf, &ctx).unwrap(),
        Value::Bool(true)
    );
    // Literal text makes it a template, and a template is string-valued.
    assert_eq!(
        evaluate(r#"{{ env.T != "" }}!"#, &vf, &ctx).unwrap(),
        Value::String("true!".into())
    );
}

#[test]
fn single_tag_keeps_number_and_object() {
    let vf = VirtualFields::defaults_only();
    let env = HashMap::new();
    let data = data_of(&[("n.v", json!(42)), ("p", json!({"a": 1, "b": "two"}))]);
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };

    // A number stays a number in the single-tag form, exactly as bare.
    assert_eq!(evaluate("{{ cache.n.v }}", &vf, &ctx).unwrap(), json!(42));
    assert_eq!(evaluate("cache.n.v", &vf, &ctx).unwrap(), json!(42));
    // …and a template stringifies it.
    assert_eq!(
        evaluate("v={{ cache.n.v }}", &vf, &ctx).unwrap(),
        Value::String("v=42".into())
    );

    // A whole-provider ref (`cache.p`) is an interior node, and an interior node
    // resolves to its subtree as an object (canon invariant 12). The single-tag
    // form takes the same typed path as the bare form — it is never re-rendered
    // as a template — so the two are identical.
    let bare = evaluate("cache.p", &vf, &ctx).unwrap();
    let tagged = evaluate("{{ cache.p }}", &vf, &ctx).unwrap();
    assert_eq!(tagged, json!({"a": 1, "b": "two"}));
    assert_eq!(tagged, bare, "the single-tag form must take the typed path");
    // A sequence keeps its shape too, and nesting survives.
    assert_eq!(
        evaluate("{{ [1, cache.p] }}", &vf, &ctx).unwrap(),
        json!([1, {"a": 1, "b": "two"}])
    );
    // A filter that yields an iterable rather than a sequence keeps its shape
    // as well, so `[1, 2]` and `[1, 2] | reverse` do not disagree about type.
    assert_eq!(
        evaluate("{{ {'a': 1} | items }}", &vf, &ctx).unwrap(),
        json!([["a", 1]])
    );
    assert_eq!(
        evaluate("{{ [1, 2] | reverse }}", &vf, &ctx).unwrap(),
        json!([2, 1])
    );

    // Literal text is what turns an interior node into a string.
    assert!(matches!(
        evaluate("p={{ cache.p }}", &vf, &ctx).unwrap(),
        Value::String(_)
    ));
}

// ── Template form is string-valued ────────────────────────────────────────────

#[test]
fn template_yields_string() {
    let vf = VirtualFields::defaults_only();
    let env = HashMap::new();
    let data = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };

    assert_eq!(
        evaluate("{{ 1 + 1 }} apples", &vf, &ctx).unwrap(),
        Value::String("2 apples".into())
    );
    // Two tags, no literal text — still a template, still a string.
    assert_eq!(
        evaluate("{{ 1 + 1 }}{{ 2 + 2 }}", &vf, &ctx).unwrap(),
        Value::String("24".into())
    );
    // A broken tag is the template compiler's diagnostic, not the expression
    // compiler's.
    let err = evaluate("{{ x } }}", &vf, &ctx).unwrap_err();
    assert!(err.starts_with("template compile error:"), "got: {err}");
}

#[test]
fn empty_source_renders_empty() {
    // An empty source is a template of no tags, not an expression: it renders
    // to "" rather than failing to compile. `-f ''` asks for exactly this.
    let vf = VirtualFields::defaults_only();
    let env = HashMap::new();
    let data = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };

    assert_eq!(classify(""), Form::Template);
    assert_eq!(
        evaluate("", &vf, &ctx).unwrap(),
        Value::String(String::new())
    );
    assert_eq!(
        evaluate("   ", &vf, &ctx).unwrap(),
        Value::String("   ".into()),
        "whitespace-only is literal template text, and survives"
    );
    assert_eq!(daemon_refs("", &vf), vec![]);
}

#[test]
fn template_with_if_over_virtual_field() {
    // git.branch is virtual; git.dirty is daemon-backed. The statement tag makes
    // the whole thing a template.
    let vf = vfields(&[("git", "branch", "env.BRANCH or cache.git.branch")]);
    let env = HashMap::new();
    let src = "{{ git.branch }}{% if git.dirty %}*{% endif %}";

    let data = data_of(&[("git.branch", json!("main")), ("git.dirty", json!(true))]);
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };
    assert_eq!(
        evaluate(src, &vf, &ctx).unwrap(),
        Value::String("main*".into())
    );

    let data = data_of(&[("git.branch", json!("main")), ("git.dirty", json!(false))]);
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };
    assert_eq!(
        evaluate(src, &vf, &ctx).unwrap(),
        Value::String("main".into())
    );

    // The env override inside the virtual field still wins.
    let env = env_of(&[("BRANCH", "feature")]);
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };
    assert_eq!(
        evaluate(src, &vf, &ctx).unwrap(),
        Value::String("feature".into())
    );
}

// ── The bare form is exactly the single-tag form ──────────────────────────────

/// Providers the built-in defaults declare, read back off the generated config's
/// `[providers.<name>]` headers. `VirtualFields` enumerates fields per provider
/// but not providers, and hardcoding the list here would let a new built-in
/// provider go silently uncovered.
fn default_providers() -> Vec<String> {
    let toml_str = VirtualFields::defaults_only().to_config_toml();
    let mut names: Vec<String> = toml_str
        .lines()
        .filter_map(|l| l.strip_prefix("[providers.")?.strip_suffix(']'))
        .map(|s| s.to_string())
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no providers found in:\n{toml_str}");
    names
}

/// A context that gives every ref in `expr` a value, so the property compares
/// two successful evaluations rather than two identical failures.
///
/// Each cached field gets its own name as its value; each whole-provider object
/// is keyed by `"v"` (every env var's value) and by `"default"` (the built-in
/// fallback key), so a selector always indexes a key that exists. The object
/// also carries `active_config`, which a `cache.P.F` ref names too — the two
/// disagree on purpose, pinning `build_context_json`'s documented precedence:
/// the whole object wins.
fn context_for(expr: &str, with_env: bool) -> (HashMap<String, String>, HashMap<String, Value>) {
    let mut env: HashMap<String, String> = HashMap::new();
    let mut data: HashMap<String, Value> = HashMap::new();
    let variant = json!({"region": "r", "project": "pr", "account": "ac"});
    for r in discover_refs(expr) {
        match r {
            Ref::Env(v) => {
                if with_env {
                    env.insert(v, "v".to_string());
                }
            }
            Ref::CacheField(p, f) | Ref::Resolved(p, f) => {
                data.insert(format!("{p}.{f}"), json!(format!("{p}.{f}")));
            }
            Ref::CacheProvider(p) => {
                data.insert(
                    p,
                    json!({"v": variant, "default": variant, "active_config": "v"}),
                );
            }
        }
    }
    (env, data)
}

#[test]
fn bare_and_single_tag_agree() {
    let vf = VirtualFields::defaults_only();
    let mut checked = 0;
    for provider in default_providers() {
        let fields = vf.fields_for(&provider);
        assert!(!fields.is_empty(), "{provider} declares no default fields");
        for field in fields {
            let expr = vf.expression(&provider, &field).unwrap().to_string();
            let tagged = format!("{{{{ {expr} }}}}");
            assert_eq!(classify(&expr), Form::Expression);
            assert_eq!(classify(&tagged), Form::SingleTag);

            for with_env in [true, false] {
                let (env, data) = context_for(&expr, with_env);
                let ctx = EvalContext {
                    env_vars: &env,
                    daemon_data: &data,
                };
                let bare = evaluate(&expr, &vf, &ctx);
                let single = evaluate(&tagged, &vf, &ctx);
                assert!(
                    bare.is_ok(),
                    "{provider}.{field} (env={with_env}) failed: {bare:?}"
                );
                assert_eq!(
                    bare, single,
                    "{provider}.{field} (env={with_env}) differs between forms"
                );
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 10, "every built-in default must be covered");
}

// ── Virtual fields defined with tags ──────────────────────────────────────────

#[test]
fn virtual_field_defined_with_tags_is_typed() {
    let vf = vfields(&[
        ("x", "tagged", "{{ env.A or cache.x.y }}"),
        ("x", "bare", "env.A or cache.x.y"),
    ]);
    let env = HashMap::new();
    let data = data_of(&[("x.y", json!(7))]);
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };

    // The gap Task 2 closes: the deps of a tag-written virtual field were
    // already discovered, but the field itself failed to compile.
    assert_eq!(
        daemon_refs("{{ x.tagged }}", &vf),
        vec![Ref::CacheField("x".into(), "y".into())]
    );

    let tagged = vf
        .evaluate("x", "tagged", &ctx, &mut HashSet::new())
        .unwrap();
    let bare = vf.evaluate("x", "bare", &ctx, &mut HashSet::new()).unwrap();
    assert_eq!(tagged, json!(7), "the tag form must keep the number");
    assert_eq!(tagged, bare);

    // …and the same through a reference to the field.
    assert_eq!(evaluate("{{ x.tagged }}", &vf, &ctx).unwrap(), json!(7));
    assert_eq!(evaluate("x.tagged", &vf, &ctx).unwrap(), json!(7));
}

#[test]
fn virtual_field_template_form_is_string() {
    let vf = vfields(&[(
        "git",
        "label",
        "{{ git.branch }}{% if git.dirty %}*{% endif %}",
    )]);
    let env = HashMap::new();
    let data = data_of(&[("git.branch", json!("main")), ("git.dirty", json!(true))]);
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };

    assert_eq!(
        vf.evaluate("git", "label", &ctx, &mut HashSet::new())
            .unwrap(),
        Value::String("main*".into())
    );
    // A template-valued virtual field referenced from another expression is a
    // string there too.
    assert_eq!(
        evaluate("{{ git.label }}", &vf, &ctx).unwrap(),
        Value::String("main*".into())
    );
}

#[test]
fn virtual_field_cycle_still_detected_through_tags() {
    let env = HashMap::new();
    let data = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };

    // Single-tag form on both sides.
    let vf = vfields(&[("a", "x", "{{ b.y }}"), ("b", "y", "{{ a.x }}")]);
    let err = vf
        .evaluate("a", "x", &ctx, &mut HashSet::new())
        .unwrap_err();
    assert!(err.contains("cycle detected"), "got: {err}");
    let err = evaluate("{{ a.x }}", &vf, &ctx).unwrap_err();
    assert!(err.contains("cycle detected"), "got: {err}");

    // Template form on both sides — the stack threads through the template
    // path's context build too.
    let vf = vfields(&[("a", "x", "[{{ b.y }}]"), ("b", "y", "[{{ a.x }}]")]);
    let err = vf
        .evaluate("a", "x", &ctx, &mut HashSet::new())
        .unwrap_err();
    assert!(err.contains("cycle detected"), "got: {err}");
    let err = evaluate("[{{ a.x }}]", &vf, &ctx).unwrap_err();
    assert!(err.contains("cycle detected"), "got: {err}");
}

// ── Generated config ──────────────────────────────────────────────────────────

#[test]
fn to_config_toml_writes_tags() {
    let vf = VirtualFields::defaults_only();
    let toml_str = vf.to_config_toml();

    // The documented form, verbatim.
    assert!(
        toml_str.contains(
            r#"virtual.workspace = "{{ env.TF_WORKSPACE or cache.terraform.workspace }}""#
        ),
        "got:\n{toml_str}"
    );
    // Quotes inside the expression stay TOML-escaped inside the tag.
    assert!(
        toml_str.contains(r#"virtual.signed_in = "{{ env.OP_SERVICE_ACCOUNT_TOKEN != \"\" }}""#),
        "got:\n{toml_str}"
    );

    // Parse the generated config the way the daemon's config loader does, feed
    // the result back through `with_config_overrides`, and check the values that
    // come out: every one is the single-tag form of the built-in expression, so
    // regenerating a config and reading it back yields the same typed field.
    let parsed: toml::Value = toml::from_str(&toml_str).expect("generated config must be TOML");
    let providers = parsed["providers"]
        .as_table()
        .expect("[providers] table")
        .clone();

    let mut overrides: Vec<((String, String), String)> = Vec::new();
    for (provider, table) in &providers {
        let virtuals = table["virtual"].as_table().expect("virtual sub-table");
        for (field, value) in virtuals {
            let src = value.as_str().expect("expression is a string").to_string();
            overrides.push(((provider.clone(), field.clone()), src));
        }
    }
    assert_eq!(
        overrides.len(),
        10,
        "every built-in default must be written"
    );

    let round_tripped = VirtualFields::with_config_overrides(overrides);
    let mut checked = 0;
    for provider in default_providers() {
        for field in round_tripped.fields_for(&provider) {
            let read_back = round_tripped.expression(&provider, &field).unwrap();
            assert_eq!(
                classify(read_back),
                Form::SingleTag,
                "{provider}.{field} did not read back as a single tag: {read_back:?}"
            );
            assert_eq!(
                single_tag_expression(read_back),
                vf.expression(&provider, &field),
                "{provider}.{field} tag body differs from the built-in expression"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 10);
}

#[test]
fn to_config_toml_leaves_an_override_that_already_has_tags_alone() {
    // Only a bare expression is wrapped. A source that already carries tags —
    // single-tag or template — is emitted as written, so an override survives a
    // regenerate instead of becoming `{{ {{ x }} }}`.
    let vf = vfields(&[
        ("x", "single", "{{ env.A or cache.x.y }}"),
        (
            "x",
            "tmpl",
            "{{ git.branch }}{% if git.dirty %}*{% endif %}",
        ),
        ("x", "plain", "env.A"),
    ]);
    let toml_str = vf.to_config_toml();

    assert!(
        toml_str.contains(r#"virtual.single = "{{ env.A or cache.x.y }}""#),
        "got:\n{toml_str}"
    );
    assert!(
        toml_str.contains(r#"virtual.tmpl = "{{ git.branch }}{% if git.dirty %}*{% endif %}""#),
        "got:\n{toml_str}"
    );
    assert!(
        toml_str.contains(r#"virtual.plain = "{{ env.A }}""#),
        "got:\n{toml_str}"
    );
    assert!(!toml_str.contains("{{ {{"), "double-wrapped:\n{toml_str}");
}

// ── Context assembly ──────────────────────────────────────────────────────────

#[test]
fn whole_provider_object_wins_over_a_disagreeing_cache_field() {
    // `cache.p` and `cache.p.f` are separate daemon queries and can disagree —
    // they are fetched at different moments. The whole object is the
    // authoritative snapshot, and that holds whichever ref the context binds
    // first, so the resolved value is the same every run.
    let vf = VirtualFields::defaults_only();
    let env = HashMap::new();
    let data = data_of(&[("p", json!({"f": "whole"})), ("p.f", json!("field"))]);
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };

    // The field ref alone: nothing to override it, so its own value stands.
    assert_eq!(
        evaluate("{{ cache.p.f }}", &vf, &ctx).unwrap(),
        json!("field")
    );

    // Both refs present, written in either order: the object supplies `f`.
    assert_eq!(
        evaluate("{{ cache.p.f or cache.p }}", &vf, &ctx).unwrap(),
        json!("whole")
    );
    assert_eq!(
        evaluate("{{ cache.p and cache.p.f }}", &vf, &ctx).unwrap(),
        json!("whole")
    );
    // …and the object itself is not damaged by the field binding.
    assert_eq!(
        evaluate("{{ cache.p.f and cache.p }}", &vf, &ctx).unwrap(),
        json!({"f": "whole"})
    );

    // A field the object does not carry is still filled in by its own ref.
    let data = data_of(&[("p", json!({"other": 1})), ("p.f", json!("field"))]);
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };
    assert_eq!(
        evaluate("{{ cache.p.f or cache.p }}", &vf, &ctx).unwrap(),
        json!("field")
    );
}

// ── Missing references ────────────────────────────────────────────────────────

#[test]
fn undefined_ref_renders_empty_in_both_forms() {
    let vf = VirtualFields::defaults_only();
    let env = HashMap::new();
    let data = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };

    // Typed path: a missing ref is the empty string, bare and tagged alike.
    assert_eq!(
        evaluate("missing.field", &vf, &ctx).unwrap(),
        Value::String(String::new())
    );
    assert_eq!(
        evaluate("{{ missing.field }}", &vf, &ctx).unwrap(),
        Value::String(String::new())
    );

    // Template path: empty, not the word "none", and not an error.
    assert_eq!(
        evaluate("[{{ missing.field }}]", &vf, &ctx).unwrap(),
        Value::String("[]".into())
    );
    assert_eq!(
        evaluate("[{{ env.NOPE }}]", &vf, &ctx).unwrap(),
        Value::String("[]".into())
    );

    // Lenient undefined behaviour is shared, so a cascade over a missing ref
    // agrees across all three forms.
    assert_eq!(
        evaluate(r#"env.MISSING or "x""#, &vf, &ctx).unwrap(),
        Value::String("x".into())
    );
    assert_eq!(
        evaluate(r#"{{ env.MISSING or "x" }}"#, &vf, &ctx).unwrap(),
        Value::String("x".into())
    );
    assert_eq!(
        evaluate(r#"[{{ env.MISSING or "x" }}]"#, &vf, &ctx).unwrap(),
        Value::String("[x]".into())
    );
}

#[test]
fn template_missing_ref_renders_empty_not_none() {
    // A ref with no value binds nothing at all, so it is undefined and writes
    // nothing. A ref that resolved to an explicit JSON null would render as
    // MiniJinja's `none` — the literal text "none" — and the template form
    // writes nothing for that too: the same "missing is falsy" reading canon
    // gives `env.*` misses and `render::render_data` gives an explicit null.
    let vf = VirtualFields::defaults_only();
    let env = HashMap::new();
    let data = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };

    let out = evaluate("branch={{ missing.field }};", &vf, &ctx).unwrap();
    assert_eq!(out, Value::String("branch=;".into()));
    assert!(!out.as_str().unwrap().contains("none"), "got: {out}");

    // Directly, over a context that carries an explicit null.
    let rendered = render_template("[{{ a.b }}]", &json!({"a": {"b": Value::Null}})).unwrap();
    assert_eq!(rendered, "[]");
    assert!(!rendered.contains("none"), "got: {rendered}");
}

// ── render_template over an already-assembled context ─────────────────────────

#[test]
fn render_template_over_assembled_context() {
    let ctx = json!({"git": {"branch": "main", "dirty": true, "tag": Value::Null}});

    assert_eq!(
        render_template("{{ git.branch }}{% if git.dirty %}*{% endif %}", &ctx).unwrap(),
        "main*"
    );
    // A null in the assembled context renders as nothing.
    assert_eq!(render_template("[{{ git.tag }}]", &ctx).unwrap(), "[]");

    // An unbound name is undefined, and chaining through it stays undefined —
    // not a render error (see `build_expression_env`'s `Chainable`).
    assert_eq!(
        render_template("[{{ nope.field }}]", &json!({})).unwrap(),
        "[]"
    );

    // Errors are prefixed by phase.
    let err = render_template("{{ x } }}", &ctx).unwrap_err();
    assert!(err.starts_with("template compile error:"), "got: {err}");
    let err = render_template("{% for x in 5 %}{% endfor %}", &json!({})).unwrap_err();
    assert!(err.starts_with("template render error:"), "got: {err}");
}

// ── A miss is undefined, not none ─────────────────────────────────────────────

#[test]
fn missing_ref_leaves_key_undefined_so_default_fires() {
    // `default` replaces an *undefined* value, not a null one, so a miss has to
    // leave the key absent from the assembled context. Binding it to JSON null
    // would make every `| default(...)` in the wild silently render empty.
    let vf = VirtualFields::defaults_only();
    let env = HashMap::new();
    let data = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };

    for src in [
        r#"{{ probe.missing | default("FB") }}"#,
        r#"probe.missing | default("FB")"#,
    ] {
        assert_eq!(
            evaluate(src, &vf, &ctx).unwrap(),
            Value::String("FB".into()),
            "src: {src}"
        );
    }
    // The template form too, and for a `cache.*` ref and a whole-provider ref.
    assert_eq!(
        evaluate(r#"[{{ probe.missing | default("FB") }}]"#, &vf, &ctx).unwrap(),
        Value::String("[FB]".into())
    );
    assert_eq!(
        evaluate(r#"{{ cache.probe.missing | default("FB") }}"#, &vf, &ctx).unwrap(),
        Value::String("FB".into())
    );
    assert_eq!(
        evaluate(r#"{{ cache.probe | default("FB") }}"#, &vf, &ctx).unwrap(),
        Value::String("FB".into())
    );

    // A hit still wins over the default, and a sibling hit lands even though
    // the other field of the same provider missed.
    let data = data_of(&[("probe.here", json!("V"))]);
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };
    assert_eq!(
        evaluate(r#"{{ probe.here | default("FB") }}"#, &vf, &ctx).unwrap(),
        Value::String("V".into())
    );
    assert_eq!(
        evaluate(
            r#"{{ probe.here }}/{{ probe.missing | default("FB") }}"#,
            &vf,
            &ctx
        )
        .unwrap(),
        Value::String("V/FB".into())
    );
}

#[test]
fn nested_access_on_missing_ref_renders_empty() {
    // Canon: a missing ref is falsy — at any depth. Under `Lenient` this was an
    // "undefined value" error; `Chainable` makes the chain yield undefined.
    let vf = VirtualFields::defaults_only();
    let env = HashMap::new();
    let data = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };

    assert_eq!(
        evaluate("{{ probe.missing.sub }}", &vf, &ctx).unwrap(),
        Value::String(String::new())
    );
    assert_eq!(
        evaluate("[{{ probe.missing.sub }}]", &vf, &ctx).unwrap(),
        Value::String("[]".into())
    );
    // Still falsy, so a cascade past it picks the next arm.
    assert_eq!(
        evaluate(r#"{{ probe.missing.sub or "next" }}"#, &vf, &ctx).unwrap(),
        Value::String("next".into())
    );
}

#[test]
fn provider_miss_with_sibling_field_ref_still_lets_default_fire() {
    // A miss must bind nothing at all — including the object that would have
    // enclosed it. Pre-creating an empty `cache.nope` / `nope` map to hold the
    // missed field would make the whole-provider ref *defined* (an empty map),
    // so `default` would not fire and the ref would render `{}`.
    let vf = VirtualFields::defaults_only();
    let env = HashMap::new();
    let data = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };

    assert_eq!(
        evaluate(
            r#"{{ cache.nope.f }}|{{ cache.nope | default("FB") }}"#,
            &vf,
            &ctx
        )
        .unwrap(),
        Value::String("|FB".into())
    );
    assert_eq!(
        evaluate(r#"{{ p.f }}|{{ p | default("FB") }}"#, &vf, &ctx).unwrap(),
        Value::String("|FB".into())
    );

    // A hit still binds, and the map it creates is the real one — the
    // whole-provider ref sees the field that hit, not an empty placeholder.
    // (`cache.P.F` is keyed `"P.F"`; `cache.P` is keyed `"P"`.)
    let data = data_of(&[("nope.f", json!("V")), ("nope", json!({"f": "V"}))]);
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };
    assert_eq!(
        evaluate(
            r#"{{ cache.nope.f }}|{{ cache.nope | default("FB") }}"#,
            &vf,
            &ctx
        )
        .unwrap(),
        Value::String(r#"V|{"f": "V"}"#.into())
    );
}

#[test]
fn virtual_field_error_names_the_field() {
    // A broken expression in a config virtual field has to say which field is
    // broken — a bare "expression compile error: ..." points at nothing.
    let vf = vfields(&[("bad", "oops", "this is ? not an expression")]);
    let env = HashMap::new();
    let data = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &data,
    };

    let err = evaluate("{{ bad.oops }}", &vf, &ctx).unwrap_err();
    assert!(err.starts_with("bad.oops: "), "got: {err}");
    assert!(err.contains("expression compile error"), "got: {err}");

    // The self-cycle error already names the field; it is not prefixed twice.
    let vf = vfields(&[("loop", "self", "loop.self")]);
    let err = evaluate("{{ loop.self }}", &vf, &ctx).unwrap_err();
    assert_eq!(
        err,
        "loop.self: virtual field cycle detected: loop.self references itself"
    );
    assert_eq!(err.matches("loop.self").count(), 2, "got: {err}");
}
