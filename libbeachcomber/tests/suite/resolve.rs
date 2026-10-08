//! The one client-side resolver, against a map-backed `Fetch`.
//!
//! `libbeachcomber::resolve` opens no socket, reads no environment and parses
//! no configuration: every input arrives in `Inputs`, every daemon answer
//! through `Fetch`. So the whole of it is testable against a `HashMap` of
//! `(key, path) → Got` — which is what this file is. The fixtures are the
//! design's own (`docs/superpowers/specs/2026-09-05-one-tree-resolution-design.md`
//! §3.1 templates, §4.2 worked example).

use libbeachcomber::resolve::{
    Fetch, FetchError, Got, Inputs, ResolveError, Slot, deps, eval, resolve, resolve_with_deps,
};
use serde_json::{Value, json};
use std::collections::HashMap;

const CWD: &str = "/Users/j/ws/foo";

// ── A map-backed Fetch ────────────────────────────────────────────────────────

/// One call to [`Fetch::get`], recorded in order.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Call {
    key: String,
    path: Option<String>,
    force: bool,
    wait: bool,
}

#[derive(Default)]
struct MapFetch {
    entries: HashMap<(String, Option<String>), Got>,
    /// Keys whose fetch fails, standing in for a transport failure.
    fails: Vec<String>,
    /// Keys the daemon refuses, standing in for `ok: false`.
    rejects: Vec<String>,
    log: Vec<Call>,
}

impl MapFetch {
    fn seed(&mut self, key: &str, path: Option<&str>, got: Got) -> &mut Self {
        self.entries
            .insert((key.to_string(), path.map(str::to_string)), got);
        self
    }

    /// The keys fetched, in call order — duplicates included, so a test can
    /// assert a key was asked for exactly once.
    fn keys(&self) -> Vec<&str> {
        self.log.iter().map(|c| c.key.as_str()).collect()
    }
}

impl Fetch for MapFetch {
    fn get(
        &mut self,
        key: &str,
        path: Option<&str>,
        force: bool,
        wait: bool,
    ) -> Result<Option<Got>, FetchError> {
        self.log.push(Call {
            key: key.to_string(),
            path: path.map(str::to_string),
            force,
            wait,
        });
        if self.fails.iter().any(|k| k == key) {
            return Err(FetchError::Transport(format!("transport failed for {key}")));
        }
        if self.rejects.iter().any(|k| k == key) {
            return Err(FetchError::Rejected(format!("invalid facet key: {key}")));
        }
        Ok(self
            .entries
            .get(&(key.to_string(), path.map(str::to_string)))
            .cloned())
    }
}

// ── Got constructors ──────────────────────────────────────────────────────────

fn data(d: Value) -> Got {
    Got {
        data: Some(d),
        age_ms: Some(812),
        stale: Some(false),
        ..Got::default()
    }
}

fn tmpl(t: Value) -> Got {
    Got {
        template: Some(t),
        ..Got::default()
    }
}

fn tmpl_rest(t: Value, rest: &str) -> Got {
    Got {
        template: Some(t),
        rest: Some(rest.to_string()),
        ..Got::default()
    }
}

fn both(d: Value, t: Value) -> Got {
    Got {
        data: Some(d),
        template: Some(t),
        age_ms: Some(30),
        stale: Some(false),
        rest: None,
    }
}

// ── Inputs ────────────────────────────────────────────────────────────────────

fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn inputs(env: &HashMap<String, String>) -> Inputs<'_> {
    Inputs {
        cwd: CWD,
        env,
        slot: Slot::Cwd,
        force: false,
        wait: false,
        allow_exec: false,
    }
}

// ── The §3.1 / §4.2 fixture ───────────────────────────────────────────────────

const THEME: &str = "{{ env.SCANLINE_THEME or 'dark' }}";
const FOO: &str = "{{ env.SCANLINE_FOO }}";
const BAR: &str = "{{ comb.git.branch | truncate(12) }}";
const WORKSPACE: &str = "{{ env.TF_WORKSPACE or comb.terraform.workspace }}";
const STATS: &str = "{{ {'cpu': 40, 'mem': 12} }}";

/// The branch `truncate(12)` cuts. The design table writes the truncation as
/// `feature/long`; the `truncate` filter this crate ships appends an ellipsis
/// past the cut (`libbeachcomber/src/filters.rs`), so the value is
/// `feature/long...`.
const BRANCH: &str = "feature/long-name-here";
const BRANCH_12: &str = "feature/long...";

fn spec_fetch() -> MapFetch {
    let widgets = json!({ "foo": FOO, "bar": BAR });
    let mut f = MapFetch::default();
    let p = Some(CWD);
    f.seed(
        "scanline",
        p,
        both(
            json!({ "lit": 1 }),
            json!({ "theme": THEME, "widgets": widgets }),
        ),
    )
    .seed("scanline.widgets", p, tmpl(widgets))
    .seed("scanline.widgets.foo", p, tmpl(json!(FOO)))
    .seed("scanline.widgets.bar", p, tmpl(json!(BAR)))
    .seed("scanline.lit", p, data(json!(1)))
    .seed("scanline.theme", p, tmpl(json!(THEME)))
    .seed("git.branch", p, data(json!(BRANCH)))
    .seed(
        "terraform.workspace",
        p,
        both(json!("default"), json!(WORKSPACE)),
    )
    .seed("scanline.stats.cpu", p, tmpl_rest(json!(STATS), "cpu"))
    .seed("scanline.stats.nope", p, tmpl_rest(json!(STATS), "nope"))
    .seed(
        "scanline.stats.cpu.deep",
        p,
        tmpl_rest(json!(STATS), "cpu.deep"),
    )
    .seed(
        "scanline.list.0",
        p,
        tmpl_rest(json!("{{ ['a','b'] }}"), "0"),
    );
    f
}

fn spec_env() -> HashMap<String, String> {
    // TF_WORKSPACE and SCANLINE_THEME deliberately unset.
    env_of(&[("SCANLINE_FOO", "hi")])
}

/// Resolve `key` against the §3.1 fixture, asserting a hit.
fn hit(key: &str) -> Value {
    let env = spec_env();
    let mut f = spec_fetch();
    resolve(key, &inputs(&env), &mut f)
        .unwrap_or_else(|e| panic!("{key} must resolve: {e}"))
        .unwrap_or_else(|| panic!("{key} must be a hit"))
}

// ── 1. Every row of the §4.2 table ────────────────────────────────────────────

#[test]
fn a_leaf_template_renders_from_the_env() {
    assert_eq!(hit("scanline.widgets.foo"), json!("hi"));
}

#[test]
fn a_leaf_template_renders_from_a_comb_ref() {
    assert_eq!(hit("scanline.widgets.bar"), json!(BRANCH_12));
}

#[test]
fn a_group_renders_every_leaf_and_keeps_its_shape() {
    assert_eq!(
        hit("scanline.widgets"),
        json!({ "foo": "hi", "bar": BRANCH_12 })
    );
}

#[test]
fn a_namespace_merges_the_rendered_tree_over_its_data() {
    assert_eq!(
        hit("scanline"),
        json!({
            "lit": 1,
            "theme": "dark",
            "widgets": { "foo": "hi", "bar": BRANCH_12 },
        })
    );
}

#[test]
fn data_with_no_template_is_returned_as_is() {
    assert_eq!(hit("scanline.lit"), json!(1));
    assert_eq!(hit("git.branch"), json!(BRANCH));
}

#[test]
fn a_template_over_data_reads_its_own_cached_value_through_cache() {
    // The cascade's first arm (env.TF_WORKSPACE) is unset, so the second
    // (comb.terraform.workspace) answers — the very `data` half the same
    // response carried, fetched once.
    let env = spec_env();
    let mut f = spec_fetch();
    let v = resolve("terraform.workspace", &inputs(&env), &mut f).unwrap();
    assert_eq!(v, Some(json!("default")));
    assert_eq!(f.keys(), vec!["terraform.workspace"]);
}

#[test]
fn an_unknown_key_is_a_miss() {
    let env = spec_env();
    let mut f = spec_fetch();
    assert_eq!(resolve("scanline.nope", &inputs(&env), &mut f), Ok(None));
}

#[test]
fn an_address_runs_past_a_template_into_what_it_renders() {
    assert_eq!(hit("scanline.stats.cpu"), json!(40));
}

#[test]
fn an_address_past_a_template_can_miss_inside_the_rendered_object() {
    let env = spec_env();
    let mut f = spec_fetch();
    assert_eq!(
        resolve("scanline.stats.nope", &inputs(&env), &mut f),
        Ok(None)
    );
}

// ── 2. Walking a rendered value ───────────────────────────────────────────────

#[test]
fn the_walk_indexes_a_rendered_array_by_decimal_index() {
    assert_eq!(hit("scanline.list.0"), json!("a"));
}

#[test]
fn the_walk_accepts_only_the_canonical_decimal_index() {
    // One spelling per index: ASCII digits, no leading zero unless the
    // segment is exactly "0". Anything else misses.
    let env = spec_env();
    let mut f = spec_fetch();
    for rest in ["+0", "01", "1 ", ""] {
        f.seed(
            &format!("scanline.list.{rest}"),
            Some(CWD),
            tmpl_rest(json!("{{ ['a','b'] }}"), rest),
        );
        assert_eq!(
            resolve(&format!("scanline.list.{rest}"), &inputs(&env), &mut f),
            Ok(None),
            "index {rest:?} must miss"
        );
    }
}

#[test]
fn the_walk_misses_on_a_scalar_with_address_left_over() {
    let env = spec_env();
    let mut f = spec_fetch();
    assert_eq!(
        resolve("scanline.stats.cpu.deep", &inputs(&env), &mut f),
        Ok(None)
    );
}

// ── 3. One fetch per distinct (key, path) ─────────────────────────────────────

#[test]
fn a_key_two_leaves_share_is_fetched_once_and_reported_once() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "scanline",
        Some(CWD),
        tmpl(json!({
            "a": "{{ comb.git.branch }}",
            "b": "{{ comb.git.branch | truncate(4) }}",
        })),
    )
    .seed("git.branch", Some(CWD), data(json!(BRANCH)));

    let (value, d) = resolve_with_deps("scanline", &inputs(&env), &mut f).unwrap();
    assert_eq!(value, Some(json!({ "a": BRANCH, "b": "feat..." })));
    assert_eq!(f.keys(), vec!["scanline", "git.branch"]);
    assert_eq!(
        d,
        vec![
            ("scanline".to_string(), Some(CWD.to_string())),
            ("git.branch".to_string(), Some(CWD.to_string())),
        ]
    );
}

#[test]
fn deps_are_the_distinct_keys_a_resolution_reads() {
    let env = spec_env();
    let mut f = spec_fetch();
    assert_eq!(
        deps("scanline.widgets.bar", &inputs(&env), &mut f).unwrap(),
        vec![
            ("scanline.widgets.bar".to_string(), Some(CWD.to_string())),
            ("git.branch".to_string(), Some(CWD.to_string())),
        ]
    );
}

#[test]
fn deps_of_a_plain_key_are_that_key_alone() {
    let env = spec_env();
    let mut f = spec_fetch();
    assert_eq!(
        deps("git.branch", &inputs(&env), &mut f).unwrap(),
        vec![("git.branch".to_string(), Some(CWD.to_string()))]
    );
}

#[test]
fn deps_of_a_template_with_two_refs_list_both_in_fetch_order() {
    let env = spec_env();
    let mut f = spec_fetch();
    f.seed(
        "k.two",
        Some(CWD),
        tmpl(json!("{{ comb.git.branch }}/{{ terraform.workspace }}")),
    );
    let (value, d) = resolve_with_deps("k.two", &inputs(&env), &mut f).unwrap();
    assert_eq!(value, Some(json!(format!("{BRANCH}/default"))));
    assert_eq!(
        d,
        vec![
            ("k.two".to_string(), Some(CWD.to_string())),
            ("git.branch".to_string(), Some(CWD.to_string())),
            ("terraform.workspace".to_string(), Some(CWD.to_string())),
        ]
    );
}

// ── 4. env.* is answered without a fetch ──────────────────────────────────────

#[test]
fn an_env_key_is_answered_from_inputs_with_no_fetch() {
    let env = spec_env();
    let mut f = spec_fetch();
    assert_eq!(
        resolve("env.SCANLINE_FOO", &inputs(&env), &mut f),
        Ok(Some(json!("hi")))
    );
    assert!(f.log.is_empty(), "env.* must not reach the daemon");
}

#[test]
fn an_unset_env_key_is_the_empty_string_with_no_fetch() {
    // Canon `field_resolution.md` invariant 6: an env miss yields `""`,
    // never an error and never a miss.
    let env = spec_env();
    let mut f = spec_fetch();
    assert_eq!(
        resolve("env.NOPE", &inputs(&env), &mut f),
        Ok(Some(json!("")))
    );
    assert!(f.log.is_empty());
}

#[test]
fn an_unset_env_ref_binds_the_empty_string() {
    // The same rule inside a template: `env.NOPE` is defined and empty, so
    // `default` — which replaces only an undefined value — does not fire.
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "k.defined",
        Some(CWD),
        tmpl(json!("{{ env.NOPE is defined }}")),
    )
    .seed(
        "k.default",
        Some(CWD),
        tmpl(json!("{{ env.NOPE | default('fb') }}")),
    );
    assert_eq!(
        resolve("k.defined", &inputs(&env), &mut f),
        Ok(Some(json!(true)))
    );
    assert_eq!(
        resolve("k.default", &inputs(&env), &mut f),
        Ok(Some(json!("")))
    );
}

#[test]
fn the_bare_env_namespace_is_a_miss_with_no_fetch() {
    let env = spec_env();
    let mut f = spec_fetch();
    assert_eq!(resolve("env", &inputs(&env), &mut f), Ok(None));
    assert!(f.log.is_empty());
}

// ── 5. A cache facet ref is not rendered ──────────────────────────────────────

#[test]
fn a_cache_facet_ref_is_not_rendered() {
    let env = spec_env();
    let mut f = spec_fetch();
    // A facet key carries no template half, so its answer is opaque by
    // construction: even were a field of it template-shaped text, it is data.
    let leaf = json!({"age_ms": 812, "stale": false, "source": "{{ head }}"});
    f.seed("git:cache.branch", Some(CWD), data(leaf.clone()));

    assert_eq!(
        resolve("git:cache.branch", &inputs(&env), &mut f),
        Ok(Some(leaf))
    );
    assert_eq!(f.keys(), vec!["git:cache.branch"]);
}

// ── 6. Slots and flags reach the fetch ────────────────────────────────────────

#[test]
fn each_slot_addresses_its_own_path() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed("git.branch", None, data(json!("global")))
        .seed("git.branch", Some("/x"), data(json!("at-x")))
        .seed("git.branch", Some(CWD), data(json!("at-cwd")));

    let mut global = inputs(&env);
    global.slot = Slot::Global;
    assert_eq!(
        resolve("git.branch", &global, &mut f),
        Ok(Some(json!("global")))
    );
    assert_eq!(f.log.last().unwrap().path, None);

    let mut at_x = inputs(&env);
    at_x.slot = Slot::Path("/x".to_string());
    assert_eq!(
        resolve("git.branch", &at_x, &mut f),
        Ok(Some(json!("at-x")))
    );
    assert_eq!(f.log.last().unwrap().path.as_deref(), Some("/x"));

    assert_eq!(
        resolve("git.branch", &inputs(&env), &mut f),
        Ok(Some(json!("at-cwd")))
    );
    assert_eq!(f.log.last().unwrap().path.as_deref(), Some(CWD));
}

#[test]
fn force_and_wait_are_forwarded_to_every_fetch() {
    let env = spec_env();
    let mut f = spec_fetch();
    let mut i = inputs(&env);
    i.force = true;
    i.wait = true;

    resolve("scanline.widgets.bar", &i, &mut f).unwrap();
    assert_eq!(f.keys(), vec!["scanline.widgets.bar", "git.branch"]);
    assert!(f.log.iter().all(|c| c.force && c.wait));
}

// ── 7. Cycles ─────────────────────────────────────────────────────────────────

fn cycle_fetch(a: &str, b: &str) -> MapFetch {
    let widgets = json!({ "a": a, "b": b });
    let mut f = MapFetch::default();
    f.seed("scanline.widgets", Some(CWD), tmpl(widgets))
        .seed("scanline.widgets.a", Some(CWD), tmpl(json!(a)))
        .seed("scanline.widgets.b", Some(CWD), tmpl(json!(b)));
    f
}

#[test]
fn a_leaf_referencing_its_own_group_is_a_cycle_naming_the_group() {
    let env = spec_env();
    // `a` names `b`, and `b` names the group itself — so assembling the group
    // needs the group. Asked for the group, the cycle closes on it.
    let mut f = cycle_fetch("{{ scanline.widgets.b }}", "{{ scanline.widgets }}");
    let err = resolve("scanline.widgets", &inputs(&env), &mut f).unwrap_err();
    assert_eq!(
        err,
        ResolveError::Config(
            "scanline.widgets.b: virtual field cycle detected: scanline.widgets is already being resolved"
                .to_string()
        )
    );
    // Asked for `a`, the same cycle (a → b → widgets → a) closes on `a`, and
    // the message names both the group that asked and the leaf it asked for.
    let err = resolve("scanline.widgets.a", &inputs(&env), &mut f).unwrap_err();
    assert_eq!(
        err,
        ResolveError::Config(
            "scanline.widgets: virtual field cycle detected: scanline.widgets.a is already being resolved"
                .to_string()
        )
    );
}

#[test]
fn a_leaf_referencing_a_sibling_by_full_path_is_not_a_cycle() {
    let env = env_of(&[("X", "sibling")]);
    let mut f = cycle_fetch("{{ scanline.widgets.b }}", "{{ env.X }}");
    // The ref names the sibling leaf, not the group above it.
    assert_eq!(
        resolve("scanline.widgets.a", &inputs(&env), &mut f),
        Ok(Some(json!("sibling")))
    );
    assert_eq!(
        f.keys(),
        vec!["scanline.widgets.a", "scanline.widgets.b"],
        "the group itself is never fetched"
    );
    // And the group assembles: `a` resolves `b` beneath it, no cycle.
    assert_eq!(
        resolve("scanline.widgets", &inputs(&env), &mut f),
        Ok(Some(json!({ "a": "sibling", "b": "sibling" })))
    );
}

#[test]
fn a_self_referencing_leaf_at_depth_is_a_cycle_naming_itself() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "scanline.widgets.x",
        Some(CWD),
        tmpl(json!("{{ scanline.widgets.x }}")),
    );
    assert_eq!(
        resolve("scanline.widgets.x", &inputs(&env), &mut f).unwrap_err(),
        ResolveError::Config(
            "scanline.widgets.x: virtual field cycle detected: scanline.widgets.x is already being resolved"
                .to_string()
        )
    );
}

#[test]
fn a_self_referencing_leaf_is_a_cycle_naming_itself() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed("scanline.x", Some(CWD), tmpl(json!("{{ scanline.x }}")));
    let err = resolve("scanline.x", &inputs(&env), &mut f).unwrap_err();
    assert_eq!(
        err,
        ResolveError::Config(
            "scanline.x: virtual field cycle detected: scanline.x is already being resolved"
                .to_string()
        )
    );

    // The template form sits on the same frame stack: literal text around
    // the ref changes nothing about the cycle.
    f.seed("scanline.y", Some(CWD), tmpl(json!("[{{ scanline.y }}]")));
    let err = resolve("scanline.y", &inputs(&env), &mut f).unwrap_err();
    assert_eq!(
        err,
        ResolveError::Config(
            "scanline.y: virtual field cycle detected: scanline.y is already being resolved"
                .to_string()
        )
    );
}

// ── 8. The depth limit ────────────────────────────────────────────────────────

#[test]
fn a_chain_of_distinct_keys_is_bounded_by_the_depth_limit() {
    let env = spec_env();
    let mut f = MapFetch::default();
    // 130 distinct keys, each referencing the next: no key repeats, so only
    // the depth bound stops it.
    for n in 0..130 {
        f.seed(
            &format!("chain.k{n}"),
            Some(CWD),
            tmpl(json!(format!("{{{{ chain.k{} }}}}", n + 1))),
        );
    }
    // Frames k0..k127 fill the stack; k128 is the one refused, and the error
    // names it.
    assert_eq!(
        resolve("chain.k0", &inputs(&env), &mut f).unwrap_err(),
        ResolveError::Config("chain.k128: virtual field nesting too deep (limit 128)".to_string())
    );
}

// ── 9. exec() ─────────────────────────────────────────────────────────────────

const EXEC_TMPL: &str = "{{ exec('printf', 'x') }}";

#[test]
fn exec_runs_when_the_surface_allows_it() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed("k.run", Some(CWD), tmpl(json!(EXEC_TMPL)));
    let mut i = inputs(&env);
    i.allow_exec = true;
    assert_eq!(resolve("k.run", &i, &mut f), Ok(Some(json!("x"))));
}

#[test]
fn exec_is_a_runtime_error_when_the_surface_disallows_it() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed("k.run", Some(CWD), tmpl(json!(EXEC_TMPL)));
    let err = resolve("k.run", &inputs(&env), &mut f).unwrap_err();
    match &err {
        ResolveError::Runtime(m) => assert!(
            m.contains("exec() is disabled") && m.contains("k.run"),
            "unexpected message: {m}"
        ),
        other => panic!("expected a runtime error, got {other:?}"),
    }
}

#[test]
fn exec_runs_in_the_template_form_too_and_is_refused_the_same_way() {
    // Both forms of canon invariant 14 share one environment, so the
    // surface's decision reaches a template exactly as it reaches a tag.
    let env = spec_env();
    let mut f = MapFetch::default();
    let src = "prefix {{ exec('printf', 'x') }} suffix";
    let mut i = inputs(&env);
    i.allow_exec = true;
    assert_eq!(eval(src, &i, &mut f).unwrap(), json!("prefix x suffix"));
    match eval(src, &inputs(&env), &mut f).unwrap_err() {
        ResolveError::Runtime(m) => {
            assert!(m.contains("exec() is disabled"), "unexpected message: {m}")
        }
        other => panic!("expected a runtime error, got {other:?}"),
    }
}

#[test]
fn a_failing_command_is_a_runtime_error_not_a_config_error() {
    // `libbeachcomber-ffi` reads a Config error as the caller's own source
    // being malformed; a command that exits non-zero is the moment's
    // failure, in both forms, and must never read as a compile error.
    let env = spec_env();
    let mut f = MapFetch::default();
    let mut i = inputs(&env);
    i.allow_exec = true;
    for src in [
        "{{ exec('sh', '-c', 'exit 3') }}",
        "[{{ exec('sh', '-c', 'exit 3') }}]",
    ] {
        match eval(src, &i, &mut f).unwrap_err() {
            ResolveError::Runtime(m) => {
                assert!(!m.contains("compile error"), "{src}: {m}");
            }
            other => panic!("{src}: expected a runtime error, got {other:?}"),
        }
    }
}

#[test]
fn an_exec_leaf_renders_beside_its_siblings_when_allowed() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "k",
        Some(CWD),
        tmpl(json!({ "run": EXEC_TMPL, "plain": "{{ 'ok' }}" })),
    );
    let mut i = inputs(&env);
    i.allow_exec = true;
    assert_eq!(
        resolve("k", &i, &mut f),
        Ok(Some(json!({ "run": "x", "plain": "ok" })))
    );
}

#[test]
fn a_runtime_failure_omits_its_leaf_and_keeps_its_siblings() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "k",
        Some(CWD),
        tmpl(json!({ "run": EXEC_TMPL, "plain": "{{ 'ok' }}" })),
    );
    assert_eq!(
        resolve("k", &inputs(&env), &mut f),
        Ok(Some(json!({ "plain": "ok" })))
    );
}

#[test]
fn an_exec_argument_may_be_a_ref_to_another_template() {
    // `chain.a`'s argument names `chain.b`, itself a template. The ref is
    // resolved first — rendered on the frame stack like any other, so the
    // depth limit and the cycle guard reach it — and the child sees the
    // rendered value, not the source.
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "chain.a",
        Some(CWD),
        tmpl(json!("{{ exec('printf', chain.b) }}")),
    )
    .seed("chain.b", Some(CWD), tmpl(json!("{{ 'bee' }}")));
    let mut i = inputs(&env);
    i.allow_exec = true;
    assert_eq!(resolve("chain.a", &i, &mut f), Ok(Some(json!("bee"))));
    assert_eq!(f.keys(), vec!["chain.a", "chain.b"]);
}

#[test]
fn an_omitted_leaf_is_not_shadowed_by_the_cached_data_beneath_it() {
    // A template at a cached field's path wins on read (outcome 4). When that
    // template fails at runtime the leaf is omitted — the cached `branch`
    // does not show through, which would be a fallback. Siblings the tree
    // does not own keep their cached values.
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "git",
        Some(CWD),
        both(
            json!({ "branch": "cached-branch", "dirty": false }),
            json!({ "branch": EXEC_TMPL }),
        ),
    )
    .seed(
        "git.branch",
        Some(CWD),
        both(json!("cached-branch"), json!(EXEC_TMPL)),
    );
    assert_eq!(
        resolve("git", &inputs(&env), &mut f),
        Ok(Some(json!({ "dirty": false })))
    );
    // And the single leaf is the error, not the cached value.
    assert!(matches!(
        resolve("git.branch", &inputs(&env), &mut f),
        Err(ResolveError::Runtime(_))
    ));
}

// ── 10. Compile failures ──────────────────────────────────────────────────────

const BAD: &str = "{{ 1 + }}";

#[test]
fn a_compile_failure_in_the_template_form_is_a_config_error() {
    // `BAD` is the single-tag form; literal text around it makes it a
    // template, which compiles through a different path.
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed("k.bad", Some(CWD), tmpl(json!("a {{ 1 + }} b")));
    match resolve("k.bad", &inputs(&env), &mut f).unwrap_err() {
        ResolveError::Config(m) => assert!(
            m.starts_with("k.bad: template compile error"),
            "unexpected message: {m}"
        ),
        other => panic!("expected a config error, got {other:?}"),
    }
}

#[test]
fn a_compile_failure_on_a_single_key_fails_the_query() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed("k.bad", Some(CWD), tmpl(json!(BAD)));
    match resolve("k.bad", &inputs(&env), &mut f).unwrap_err() {
        ResolveError::Config(m) => assert!(m.starts_with("k.bad: "), "unexpected message: {m}"),
        other => panic!("expected a config error, got {other:?}"),
    }
}

#[test]
fn a_compile_failure_in_one_leaf_fails_the_whole_group_naming_the_leaf() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "k",
        Some(CWD),
        tmpl(json!({ "bad": BAD, "plain": "{{ 'ok' }}" })),
    );
    match resolve("k", &inputs(&env), &mut f).unwrap_err() {
        ResolveError::Config(m) => assert!(m.starts_with("k.bad: "), "unexpected message: {m}"),
        other => panic!("expected a config error, got {other:?}"),
    }
}

// ── 11. An all-empty cascade ──────────────────────────────────────────────────

#[test]
fn an_all_empty_cascade_is_the_empty_string_alone_and_in_a_group() {
    let env = spec_env();
    let empty = "{{ env.NOPE or comb.nothing.here }}";
    let mut f = MapFetch::default();
    f.seed("k.empty", Some(CWD), tmpl(json!(empty))).seed(
        "k",
        Some(CWD),
        tmpl(json!({ "empty": empty })),
    );

    assert_eq!(
        resolve("k.empty", &inputs(&env), &mut f),
        Ok(Some(json!("")))
    );
    assert_eq!(
        resolve("k", &inputs(&env), &mut f),
        Ok(Some(json!({ "empty": "" })))
    );
}

// ── 11a. What a ref binds ─────────────────────────────────────────────────────

#[test]
fn a_whole_provider_object_wins_over_a_disagreeing_cache_field() {
    // `comb.p` and `comb.p.f` are separate daemon reads and can disagree.
    // The whole object is the authoritative snapshot, and that holds
    // whichever ref is bound first, so the value is the same every run.
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed("p", Some(CWD), data(json!({ "f": "whole" }))).seed(
        "p.f",
        Some(CWD),
        data(json!("field")),
    );
    let i = inputs(&env);

    // The field ref alone: nothing to override it, so its own value stands.
    assert_eq!(eval("{{ comb.p.f }}", &i, &mut f).unwrap(), json!("field"));
    // Both refs, written in either order: the object supplies `f`.
    assert_eq!(
        eval("{{ comb.p.f or comb.p }}", &i, &mut f).unwrap(),
        json!("whole")
    );
    assert_eq!(
        eval("{{ comb.p and comb.p.f }}", &i, &mut f).unwrap(),
        json!("whole")
    );
    // …and the object itself is not damaged by the field binding.
    assert_eq!(
        eval("{{ comb.p.f and comb.p }}", &i, &mut f).unwrap(),
        json!({ "f": "whole" })
    );

    // A field the object does not carry is still filled in by its own ref.
    f.seed("p", Some(CWD), data(json!({ "other": 1 })));
    assert_eq!(
        eval("{{ comb.p.f or comb.p }}", &i, &mut f).unwrap(),
        json!("field")
    );
}

#[test]
fn a_miss_binds_nothing_so_default_fires_at_any_depth() {
    // `default` replaces an *undefined* value, not a null one, so a miss has
    // to leave the key absent from the context — the value, and the object
    // that would have enclosed it. Binding a miss to JSON null would make
    // every `| default(...)` in the wild silently render empty; pre-creating
    // an empty `comb.nope` to hold a missed field would make the
    // whole-provider ref *defined* and render `{}`. Chaining through a miss
    // stays undefined, so it is falsy at any depth (canon).
    let env = spec_env();
    let mut f = MapFetch::default();
    let i = inputs(&env);
    for (src, want) in [
        (r#"{{ probe.missing | default("FB") }}"#, "FB"),
        (r#"[{{ probe.missing | default("FB") }}]"#, "[FB]"),
        (r#"{{ comb.probe.missing | default("FB") }}"#, "FB"),
        (r#"{{ comb.probe | default("FB") }}"#, "FB"),
        (
            r#"{{ comb.nope.f }}|{{ comb.nope | default("FB") }}"#,
            "|FB",
        ),
        (r#"{{ p.f }}|{{ p | default("FB") }}"#, "|FB"),
        ("{{ probe.missing.sub }}", ""),
        ("[{{ probe.missing.sub }}]", "[]"),
        (r#"{{ probe.missing.sub or "next" }}"#, "next"),
    ] {
        assert_eq!(eval(src, &i, &mut f).unwrap(), json!(want), "src: {src}");
    }

    // A hit still binds, and the map it creates is the real one — the
    // whole-provider ref sees the field that hit, not a placeholder.
    f.seed("nope.f", Some(CWD), data(json!("V"))).seed(
        "nope",
        Some(CWD),
        data(json!({ "f": "V" })),
    );
    assert_eq!(
        eval(
            r#"{{ comb.nope.f }}|{{ comb.nope | default("FB") }}"#,
            &i,
            &mut f
        )
        .unwrap(),
        json!(r#"V|{"f": "V"}"#)
    );

    // A resolved hit lands beside a resolved miss on the same provider: the
    // hit creates `probe`, and the missed sibling is still absent from it.
    f.seed("probe.here", Some(CWD), data(json!("V")));
    assert_eq!(
        eval(
            r#"{{ probe.here }}/{{ probe.missing | default("FB") }}"#,
            &i,
            &mut f
        )
        .unwrap(),
        json!("V/FB")
    );
}

// ── 12. The two expression forms ──────────────────────────────────────────────

#[test]
fn the_two_expression_forms_keep_their_types() {
    let env = env_of(&[("A", "set")]);
    let mut f = MapFetch::default();
    f.seed("k.text", Some(CWD), tmpl(json!("hello")))
        .seed("k.sum", Some(CWD), tmpl(json!("{{ 1 + 1 }}")))
        .seed("k.bool", Some(CWD), tmpl(json!("{{ env.A != '' }}")))
        .seed("k.mixed", Some(CWD), tmpl(json!("a {{ 1 }} b")));

    let i = inputs(&env);
    assert_eq!(resolve("k.text", &i, &mut f), Ok(Some(json!("hello"))));
    assert_eq!(resolve("k.sum", &i, &mut f), Ok(Some(json!(2))));
    assert_eq!(resolve("k.bool", &i, &mut f), Ok(Some(json!(true))));
    assert_eq!(resolve("k.mixed", &i, &mut f), Ok(Some(json!("a 1 b"))));
}

#[test]
fn the_single_tag_form_keeps_structured_shapes() {
    // An interior node resolves to its subtree as an object, a sequence
    // stays a sequence, and a filter yielding an iterable keeps that shape
    // too (canon invariants 8 and 12) — the typed path never re-renders a
    // structured result as a string. Literal text is what stringifies it.
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed("p", Some(CWD), data(json!({ "a": 1, "b": "two" })));
    let i = inputs(&env);
    assert_eq!(
        eval("{{ comb.p }}", &i, &mut f).unwrap(),
        json!({ "a": 1, "b": "two" })
    );
    assert_eq!(
        eval("{{ [1, comb.p] }}", &i, &mut f).unwrap(),
        json!([1, { "a": 1, "b": "two" }])
    );
    assert_eq!(
        eval("{{ {'a': 1} | items }}", &i, &mut f).unwrap(),
        json!([["a", 1]])
    );
    assert_eq!(
        eval("{{ [1, 2] | reverse }}", &i, &mut f).unwrap(),
        json!([2, 1])
    );
    assert!(matches!(
        eval("p={{ comb.p }}", &i, &mut f).unwrap(),
        Value::String(_)
    ));
}

#[test]
fn a_statement_tag_template_renders_over_a_template_ref_and_a_data_ref() {
    // `git.branch` is a template over the env and the cache; `git.dirty` is
    // data. The statement tag makes the source a template, and each ref in
    // it resolves exactly as `resolve` of that key would.
    let src = "{{ git.branch }}{% if git.dirty %}*{% endif %}";
    let mut f = MapFetch::default();
    f.seed(
        "git.branch",
        Some(CWD),
        both(json!("main"), json!("{{ env.BRANCH or comb.git.branch }}")),
    )
    .seed("git.dirty", Some(CWD), data(json!(true)));

    let env = spec_env();
    assert_eq!(eval(src, &inputs(&env), &mut f).unwrap(), json!("main*"));

    f.seed("git.dirty", Some(CWD), data(json!(false)));
    assert_eq!(eval(src, &inputs(&env), &mut f).unwrap(), json!("main"));

    // The env arm inside the referenced template still wins.
    let env = env_of(&[("BRANCH", "feature")]);
    assert_eq!(eval(src, &inputs(&env), &mut f).unwrap(), json!("feature"));
}

// ── 13. A transport failure ───────────────────────────────────────────────────

#[test]
fn a_fetch_error_stops_the_resolution_where_it_happened() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "k",
        Some(CWD),
        // Sorted leaf order puts `a` first; its ref fails, so `b`'s never runs.
        tmpl(json!({ "a": "{{ boom.x }}", "b": "{{ comb.git.branch }}" })),
    )
    .seed("git.branch", Some(CWD), data(json!(BRANCH)));
    f.fails.push("boom.x".to_string());

    let err = resolve("k", &inputs(&env), &mut f).unwrap_err();
    assert_eq!(
        err,
        ResolveError::Fetch(FetchError::Transport(
            "transport failed for boom.x".to_string()
        ))
    );
    assert_eq!(f.keys(), vec!["k", "boom.x"]);
}

// ── 13a. A rejected key ───────────────────────────────────────────────────────

/// The key the caller asked for being refused is an error in the daemon's
/// words; the same key named by a ref inside a template is a missing ref,
/// falsy at any depth, and asked about once however many refs name it.
#[test]
fn a_rejected_top_level_key_is_an_error_and_a_rejected_ref_is_falsy() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.rejects.push("c.z".to_string());

    assert_eq!(
        resolve("c.z", &inputs(&env), &mut f),
        Err(ResolveError::Fetch(FetchError::Rejected(
            "invalid facet key: c.z".to_string()
        )))
    );

    let mut f = MapFetch::default();
    f.rejects.push("c.z".to_string());
    f.seed("k", Some(CWD), tmpl(json!("{{ comb.c.z or c.z or 'fb' }}")));
    assert_eq!(resolve("k", &inputs(&env), &mut f), Ok(Some(json!("fb"))));
    assert_eq!(
        f.keys(),
        vec!["k", "c.z"],
        "a rejection is memoised like any other answer: two refs, one fetch"
    );

    // Inside a group, too: the leaf renders with the ref unbound rather than
    // failing the query.
    let mut f = MapFetch::default();
    f.rejects.push("c.z".to_string());
    f.seed(
        "g",
        Some(CWD),
        tmpl(json!({ "a": "{{ comb.c.z | default('fb') }}", "b": "x" })),
    );
    assert_eq!(
        resolve("g", &inputs(&env), &mut f),
        Ok(Some(json!({ "a": "fb", "b": "x" })))
    );
}

// ── 14. eval ──────────────────────────────────────────────────────────────────

#[test]
fn eval_of_a_ref_equals_resolving_that_key() {
    let env = spec_env();
    let mut f = spec_fetch();
    assert_eq!(
        eval("{{ scanline.widgets.foo }}", &inputs(&env), &mut f).unwrap(),
        json!("hi")
    );
}

#[test]
fn eval_reads_the_same_keys_as_resolving_the_ref() {
    let env = spec_env();
    let mut resolved = spec_fetch();
    let mut evaled = spec_fetch();

    // A ref names a node at any depth, so `eval` of the ref reads exactly
    // what `resolve` of the key reads.
    let by_key = deps("scanline.widgets.foo", &inputs(&env), &mut resolved).unwrap();
    eval("{{ scanline.widgets.foo }}", &inputs(&env), &mut evaled).unwrap();
    assert_eq!(
        by_key,
        vec![("scanline.widgets.foo".to_string(), Some(CWD.to_string()))]
    );
    let evaled_keys: Vec<&str> = evaled.keys();
    assert_eq!(evaled_keys, vec!["scanline.widgets.foo"]);
}

#[test]
fn a_ref_at_depth_fetches_that_leaf_and_never_the_group_above_it() {
    let env = spec_env();
    let mut f = spec_fetch();
    f.seed(
        "k.ref",
        Some(CWD),
        tmpl(json!("{{ scanline.widgets.foo }}")),
    );
    assert_eq!(
        resolve("k.ref", &inputs(&env), &mut f),
        Ok(Some(json!("hi")))
    );
    assert_eq!(f.keys(), vec!["k.ref", "scanline.widgets.foo"]);
}

#[test]
fn a_ref_past_a_template_walks_into_what_it_renders() {
    let env = spec_env();
    let mut f = spec_fetch();
    // `scanline.stats.cpu` is served as the stats template plus `rest: cpu`;
    // a ref to it takes the same walk `resolve` does.
    assert_eq!(
        eval("{{ scanline.stats.cpu }}", &inputs(&env), &mut f).unwrap(),
        json!(40)
    );
    assert_eq!(
        eval("{{ scanline.stats.cpu + 2 }}", &inputs(&env), &mut f).unwrap(),
        json!(42)
    );
}

#[test]
fn eval_of_tagless_text_is_that_text() {
    let env = spec_env();
    let mut f = spec_fetch();
    assert_eq!(
        eval("hello", &inputs(&env), &mut f).unwrap(),
        json!("hello")
    );
    // An empty source is a template of no tags — `-f ''` asks for exactly
    // this — and whitespace-only is literal text that survives.
    assert_eq!(eval("", &inputs(&env), &mut f).unwrap(), json!(""));
    assert_eq!(eval("   ", &inputs(&env), &mut f).unwrap(), json!("   "));
    assert!(f.log.is_empty());
}

// ── 15. A template half that is neither a string nor an object ────────────────

#[test]
fn a_template_half_that_is_a_number_is_a_config_error() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed("k.num", Some(CWD), tmpl(json!(42)));
    match resolve("k.num", &inputs(&env), &mut f).unwrap_err() {
        ResolveError::Config(m) => assert!(m.starts_with("k.num: "), "unexpected message: {m}"),
        other => panic!("expected a config error, got {other:?}"),
    }
}

#[test]
fn a_template_leaf_that_is_not_a_string_is_a_config_error_naming_it() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed("k", Some(CWD), tmpl(json!({ "n": 42 })));
    match resolve("k", &inputs(&env), &mut f).unwrap_err() {
        ResolveError::Config(m) => assert_eq!(m, "k.n: template leaf is not a string"),
        other => panic!("expected a config error, got {other:?}"),
    }
}
