//! The one client-side resolver, against a map-backed `Fetch`.
//!
//! `libbeachcomber::resolve` opens no socket, reads no environment and parses
//! no configuration: every input arrives in `Inputs`, every daemon answer
//! through `Fetch`. So the whole of it is testable against a `HashMap` of
//! `(key, path) → Got` — which is what this file is. The fixtures are the
//! design's own (`docs/superpowers/specs/2026-09-05-one-tree-resolution-design.md`
//! §3.1 templates, §4.2 worked example).

use libbeachcomber::resolve::{
    Fetch, FetchError, Got, Inputs, MemoWrite, ResolveError, Slot, Traced, deps, eval, eval_with,
    resolve, resolve_traced,
};
use serde_json::{Value, json};
use std::collections::HashMap;

// A real directory: an `exec` child runs in the render's cwd.
const CWD: &str = "/tmp";

// ── A map-backed Fetch ────────────────────────────────────────────────────────

/// The failure `MapFetch` reports for a key it was told fails.
fn transport_failure(key: &str) -> libbeachcomber::CombError {
    libbeachcomber::CombError::IoError(std::io::Error::other(format!("transport failed for {key}")))
}

/// One call to [`Fetch::get`], recorded in order.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Call {
    key: String,
    path: Option<String>,
    force: bool,
    wait: libbeachcomber::Wait,
}

#[derive(Default)]
struct MapFetch {
    entries: HashMap<(String, Option<String>), Got>,
    /// Keys whose fetch fails, standing in for a transport failure.
    fails: Vec<String>,
    /// Keys the daemon refuses, standing in for `ok: false`.
    rejects: Vec<String>,
    log: Vec<Call>,
    /// The memos written back, in order.
    writes: Vec<MemoWrite>,
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
        wait: libbeachcomber::Wait,
    ) -> Result<Option<Got>, FetchError> {
        self.log.push(Call {
            key: key.to_string(),
            path: path.map(str::to_string),
            force,
            wait,
        });
        if self.fails.iter().any(|k| k == key) {
            return Err(FetchError::Transport(transport_failure(key)));
        }
        if self.rejects.iter().any(|k| k == key) {
            return Err(FetchError::Rejected(format!("invalid facet key: {key}")));
        }
        Ok(self
            .entries
            .get(&(key.to_string(), path.map(str::to_string)))
            .cloned())
    }

    fn memo(&mut self, write: &MemoWrite) -> Result<(), FetchError> {
        self.writes.push(write.clone());
        Ok(())
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
        memo: None,
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
        wait: libbeachcomber::Wait::Budget,
        allow_exec: false,
        now: std::time::SystemTime::now(),
    }
}

// ── The §3.1 / §4.2 fixture ───────────────────────────────────────────────────

const THEME: &str = "{{ env.SCANLINE_THEME or 'dark' }}";
const FOO: &str = "{{ env.SCANLINE_FOO }}";
const BAR: &str = "{{ comb.git.branch | truncate(12) }}";
const WORKSPACE: &str = "{{ env.TF_WORKSPACE or comb.terraform.workspace }}";
const STATS: &str = "{{ {'cpu': 40, 'mem': 12} }}";

/// The branch `truncate(12)` cuts. The design table writes the truncation as
/// `feature/long`; the `truncate` filter this crate ships counts the `…` inside
/// the length (`libbeachcomber/src/library/measure.rs`), so the value is
/// `feature/lon…`.
const BRANCH: &str = "feature/long-name-here";
const BRANCH_12: &str = "feature/lon…";

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

/// `now()` is one value for a whole resolution: every leaf of a group reads
/// the clock the caller set, not its own.
#[test]
fn now_is_one_value_per_resolution() {
    let vars = spec_env();
    let mut f = MapFetch::default();
    let p = Some(CWD);
    let tree =
        json!({ "a": "{{ now() }}", "b": "{{ now() - 60 }}", "c": "{{ spinner('ab', '10s') }}" });
    f.seed("clock", p, tmpl(tree))
        .seed("clock.a", p, tmpl(json!("{{ now() }}")))
        .seed("clock.b", p, tmpl(json!("{{ now() - 60 }}")))
        .seed("clock.c", p, tmpl(json!("{{ spinner('ab', '10s') }}")));
    let mut i = inputs(&vars);
    i.now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_791_342_010);
    let got = resolve("clock", &i, &mut f).unwrap().unwrap();
    assert_eq!(
        got,
        json!({ "a": 1_791_342_010_i64, "b": 1_791_341_950_i64, "c": "b" })
    );
    // The next resolution is given its own.
    i.now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_791_342_020);
    let got = resolve("clock.a", &i, &mut f).unwrap().unwrap();
    assert_eq!(got, json!(1_791_342_020_i64));
}

/// A leaf whose statements come before its one `{{ }}` keeps the
/// expression's type: `{% set x = 1 %}{{ x }}` is the number 1, and a group
/// holding such a leaf holds the number.
#[test]
fn a_leaf_with_a_set_preamble_is_a_number() {
    let vars = env_of(&[("W", "80")]);
    let mut f = MapFetch::default();
    let p = Some(CWD);
    let leaf = "{%- set w = env.W | int -%}\n{%- set half = w // 2 -%}\n{{- half -}}";
    f.seed("calc", p, tmpl(json!({ "half": leaf })))
        .seed("calc.half", p, tmpl(json!(leaf)))
        .seed("calc.one", p, tmpl(json!("{% set x = 1 %}{{ x }}")));
    assert_eq!(
        resolve("calc.one", &inputs(&vars), &mut f).unwrap(),
        Some(json!(1))
    );
    assert_eq!(
        resolve("calc.half", &inputs(&vars), &mut f).unwrap(),
        Some(json!(40))
    );
    assert_eq!(
        resolve("calc", &inputs(&vars), &mut f).unwrap(),
        Some(json!({ "half": 40 }))
    );
    assert_eq!(
        eval(
            "{% set xs = [1, 2] %}{{ xs | length }}",
            &inputs(&vars),
            &mut f
        )
        .unwrap(),
        Some(json!(2))
    );
}

// ── Macro libraries ───────────────────────────────────────────────────────────

/// A library as the daemon answers `<p>:macro.<name>`: its text as data,
/// with an age that is no input's.
fn lib(text: &str) -> Got {
    data(json!(text))
}

/// `key` resolved against `f` from the fixture's directory, with `vars`.
fn resolve_with(
    key: &str,
    f: &mut MapFetch,
    vars: &HashMap<String, String>,
) -> Result<Option<Value>, ResolveError> {
    resolve(key, &inputs(vars), f)
}

const USAGE: &str = "{%- macro usage(p) -%}{{ 'red' if p > 80 else 'green' }}{%- endmacro -%}";

/// A leaf imports a macro from a library by its address and calls it.
#[test]
fn a_leaf_imports_a_macro() {
    let vars = env_of(&[]);
    let mut f = MapFetch::default();
    let p = Some(CWD);
    f.seed("s:macro.usage", p, lib(USAGE)).seed(
        "s.colour",
        p,
        tmpl(json!(
            "{% from 's:macro.usage' import usage %}{{ usage(90) ~ '/' ~ usage(10) }}"
        )),
    );
    assert_eq!(
        resolve_with("s.colour", &mut f, &vars).unwrap(),
        Some(json!("red/green"))
    );
    // `import … as` is the module form of the same library.
    f.seed(
        "s.module",
        p,
        tmpl(json!(
            "{% import \"s:macro.usage\" as u %}{{ u.usage(81) }}"
        )),
    );
    assert_eq!(
        resolve_with("s.module", &mut f, &vars).unwrap(),
        Some(json!("red"))
    );
}

/// A module import exposes the library's top-level `set`s with their types.
#[test]
fn a_module_import_exports_typed_sets() {
    let vars = env_of(&[("W", "80")]);
    let mut f = MapFetch::default();
    let p = Some(CWD);
    f.seed(
        "s:macro.k",
        p,
        lib("{%- set w = env.W | int -%}{%- set half = w // 2 -%}{%- set xs = [1, 2] -%}"),
    )
    .seed(
        "s.half",
        p,
        tmpl(json!(
            "{% import 's:macro.k' as k %}{{ k.half + k.xs | length }}"
        )),
    );
    assert_eq!(
        resolve_with("s.half", &mut f, &vars).unwrap(),
        Some(json!(42))
    );
}

/// A library imports another; each is read once.
#[test]
fn a_library_imports_a_library() {
    let vars = env_of(&[]);
    let mut f = MapFetch::default();
    let p = Some(CWD);
    f.seed("s:macro.usage", p, lib(USAGE))
        .seed(
            "s:macro.row",
            p,
            lib("{%- from 's:macro.usage' import usage -%}\
                 {%- macro row(n) -%}[{{ usage(n) }}]{%- endmacro -%}"),
        )
        .seed(
            "s.rows",
            p,
            tmpl(json!(
                "{% from 's:macro.row' import row %}{% from 's:macro.usage' import usage %}\
                 {{ row(99) ~ usage(1) }}"
            )),
        );
    assert_eq!(
        resolve_with("s.rows", &mut f, &vars).unwrap(),
        Some(json!("[red]green"))
    );
    let reads = f
        .keys()
        .iter()
        .filter(|k| k.starts_with("s:macro."))
        .count();
    assert_eq!(reads, 2, "each library once: {:?}", f.keys());
}

/// What a library reads is fetched for the render that imports it: a macro
/// body's provider ref is bound in the importer's context.
#[test]
fn a_ref_inside_a_library_is_fetched() {
    let vars = env_of(&[]);
    let mut f = MapFetch::default();
    let p = Some(CWD);
    f.seed(
        "s:macro.tag",
        p,
        lib("{%- macro tag() -%}<{{ git.branch }}>{%- endmacro -%}"),
    )
    .seed("git.branch", p, data(json!("main")))
    .seed(
        "s.tag",
        p,
        tmpl(json!("{% from 's:macro.tag' import tag %}{{ tag() }}")),
    );
    assert_eq!(
        resolve_with("s.tag", &mut f, &vars).unwrap(),
        Some(json!("<main>"))
    );
    // The importer's own names are the library's context too.
    f.seed(
        "s:macro.hi",
        p,
        lib("{%- set greeting = 'hi ' ~ who -%}{%- macro to() -%}{{ greeting }}, {{ who }}{%- endmacro -%}"),
    )
    .seed(
        "s.hi",
        p,
        tmpl(json!("{% set who = 'me' %}{% import 's:macro.hi' as h %}{{ h.greeting ~ '/' ~ h.to() }}")),
    );
    assert_eq!(
        resolve_with("s.hi", &mut f, &vars).unwrap(),
        Some(json!("hi me/hi me, me"))
    );
}

/// A `comb[...]` in a library puts the importing render on the lazy path:
/// the provider is read when the macro asks for it.
#[test]
fn a_library_reading_comb_by_subscript_is_lazy() {
    let vars = env_of(&[]);
    let mut f = MapFetch::default();
    let p = Some(CWD);
    f.seed(
        "s:macro.pick",
        p,
        lib("{%- macro pick(name) -%}{{ comb[name].branch }}{%- endmacro -%}"),
    )
    .seed("git", p, data(json!({"branch": "main"})))
    .seed(
        "s.pick",
        p,
        tmpl(json!(
            "{% from 's:macro.pick' import pick %}{{ pick('git') }}"
        )),
    );
    assert_eq!(
        resolve_with("s.pick", &mut f, &vars).unwrap(),
        Some(json!("main"))
    );
    assert!(f.keys().contains(&"git"), "{:?}", f.keys());
}

/// A library is a dependency, so a watch follows it, and is text, like a
/// template, so its age is no input's.
#[test]
fn a_library_is_a_dependency_with_no_age() {
    let vars = env_of(&[]);
    let mut f = MapFetch::default();
    let p = Some(CWD);
    f.seed("s:macro.usage", p, lib(USAGE)).seed(
        "s.c",
        p,
        tmpl(json!(
            "{% from 's:macro.usage' import usage %}{{ usage(1) }}"
        )),
    );
    let traced = resolve_traced("s.c", &inputs(&vars), &mut f).unwrap();
    assert_eq!(traced.value, Some(json!("green")));
    let deps: Vec<&str> = traced.deps.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(deps, ["s.c", "s:macro.usage"]);
    assert_eq!(traced.oldest_input_ms, None, "a library is not an input");
}

/// A library nothing answers fails the leaf that imports it, named; a group
/// omits that leaf and renders its siblings.
#[test]
fn a_missing_library_fails_only_that_leaf() {
    let vars = env_of(&[]);
    let mut f = MapFetch::default();
    let p = Some(CWD);
    let bad = "{% from 's:macro.nope' import m %}{{ m() }}";
    f.seed("s", p, tmpl(json!({"bad": bad, "ok": "{{ 1 }}"})))
        .seed("s.bad", p, tmpl(json!(bad)))
        .seed("s.ok", p, tmpl(json!("{{ 1 }}")))
        .seed("s:macro.n", p, data(json!(1)))
        .seed(
            "s.n",
            p,
            tmpl(json!("{% from 's:macro.n' import m %}{{ m() }}")),
        );
    assert_eq!(
        resolve_with("s.bad", &mut f, &vars),
        Err(ResolveError::Runtime(
            "s.bad: s:macro.nope: no macro library there".into()
        ))
    );
    assert_eq!(
        resolve_with("s", &mut f, &vars).unwrap(),
        Some(json!({"ok": 1}))
    );
    assert_eq!(
        resolve_with("s.n", &mut f, &vars),
        Err(ResolveError::Config(
            "s.n: s:macro.n: a macro library is a string, not a number".into()
        ))
    );
}

/// An import names its library as a string: a computed name is refused
/// before anything renders, and nothing is read for it.
#[test]
fn a_computed_import_name_is_refused() {
    let vars = env_of(&[]);
    let mut f = MapFetch::default();
    let p = Some(CWD);
    f.seed(
        "s.c",
        p,
        tmpl(json!(
            "{% set n = 's:macro.usage' %}{% import n as u %}{{ u.usage(1) }}"
        )),
    );
    let err = resolve_with("s.c", &mut f, &vars).unwrap_err();
    assert_eq!(
        err,
        ResolveError::Config(
            "s.c: template compile error: {% import n as u %}: an import names its library as \
             a string, \"<p>:macro.<name>\""
                .into()
        )
    );
    assert_eq!(f.keys(), ["s.c"], "nothing read for it");
}

/// Two libraries that import each other end in an error, not a hang or a
/// crash: each is read once, and the engine stops the recursion.
#[test]
fn an_import_cycle_errors_without_panicking() {
    let vars = env_of(&[]);
    let mut f = MapFetch::default();
    let p = Some(CWD);
    f.seed(
        "s:macro.a",
        p,
        lib("{% import 's:macro.b' as b %}{% macro a() %}a{% endmacro %}"),
    )
    .seed(
        "s:macro.b",
        p,
        lib("{% import 's:macro.a' as a %}{% macro b() %}b{% endmacro %}"),
    )
    .seed(
        "s.c",
        p,
        tmpl(json!("{% import 's:macro.a' as a %}{{ a.a() }}")),
    );
    assert_eq!(
        resolve_with("s.c", &mut f, &vars),
        Err(ResolveError::Runtime(
            "s.c: template render error: recursion limit exceeded (in s:macro.b:1)".into()
        ))
    );
    let reads = f
        .keys()
        .iter()
        .filter(|k| k.starts_with("s:macro."))
        .count();
    assert_eq!(reads, 2, "each read once: {:?}", f.keys());
}

/// An error raised inside a library says what went wrong and where in the
/// library, not that an import failed.
#[test]
fn an_error_in_a_library_names_its_place() {
    let vars = env_of(&[]);
    let mut f = MapFetch::default();
    let p = Some(CWD);
    f.seed("s:macro.top", p, lib("{% set x = 1 + 'a' %}"))
        .seed(
            "s:macro.body",
            p,
            lib("{% macro m() %}\n{{ 1 + 'a' }}{% endmacro %}"),
        )
        .seed(
            "s.top",
            p,
            tmpl(json!("{% import 's:macro.top' as t %}{{ t.x }}")),
        )
        .seed(
            "s.body",
            p,
            tmpl(json!("{% from 's:macro.body' import m %}{{ m() }}")),
        );
    let plus = "tried to use + operator on unsupported types number and string";
    assert_eq!(
        resolve_with("s.top", &mut f, &vars),
        Err(ResolveError::Runtime(format!(
            "s.top: template render error: {plus} (in s:macro.top:1)"
        )))
    );
    assert_eq!(
        resolve_with("s.body", &mut f, &vars),
        Err(ResolveError::Runtime(format!(
            "s.body: template render error: {plus} (in s:macro.body:2)"
        )))
    );
}

/// The nearest directory's library is the one the daemon answers at the
/// resolution's path: a put there shadows the file's at `~`.
#[test]
fn a_library_put_at_a_directory_shadows_the_file() {
    let vars = env_of(&[]);
    let mut f = MapFetch::default();
    f.seed("s:macro.usage", None, lib(USAGE))
        .seed(
            "s:macro.usage",
            Some(CWD),
            lib("{%- macro usage(p) -%}mine{%- endmacro -%}"),
        )
        .seed(
            "s.c",
            Some(CWD),
            tmpl(json!(
                "{% from 's:macro.usage' import usage %}{{ usage(1) }}"
            )),
        );
    assert_eq!(
        resolve_with("s.c", &mut f, &vars).unwrap(),
        Some(json!("mine"))
    );
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

    let Traced { value, deps: d, .. } = resolve_traced("scanline", &inputs(&env), &mut f).unwrap();
    assert_eq!(value, Some(json!({ "a": BRANCH, "b": "fea…" })));
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
    let Traced { value, deps: d, .. } = resolve_traced("k.two", &inputs(&env), &mut f).unwrap();
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
fn an_unset_env_key_is_absent_with_no_fetch() {
    // Null means absent, never `""`: an unset variable is a miss, never an
    // error.
    let env = spec_env();
    let mut f = spec_fetch();
    assert_eq!(resolve("env.NOPE", &inputs(&env), &mut f), Ok(None));
    assert!(f.log.is_empty());
}

#[test]
fn an_unset_env_ref_binds_nothing() {
    // The same rule inside a template, as in the daemon's lines: `env.NOPE`
    // is undefined, so `default` fires; a variable set to `""` is defined.
    let mut env = spec_env();
    env.insert("EMPTY".to_string(), String::new());
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
    )
    .seed(
        "k.empty",
        Some(CWD),
        tmpl(json!("{{ env.EMPTY | default('fb') }}")),
    );
    assert_eq!(
        resolve("k.defined", &inputs(&env), &mut f),
        Ok(Some(json!(false)))
    );
    assert_eq!(
        resolve("k.default", &inputs(&env), &mut f),
        Ok(Some(json!("fb")))
    );
    assert_eq!(
        resolve("k.empty", &inputs(&env), &mut f),
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
    i.wait = libbeachcomber::Wait::For(std::time::Duration::from_millis(50));

    resolve("scanline.widgets.bar", &i, &mut f).unwrap();
    assert_eq!(f.keys(), vec!["scanline.widgets.bar", "git.branch"]);
    assert!(f.log.iter().all(|c| c.force && c.wait == i.wait));
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

#[test]
fn a_node_being_rendered_is_a_cycle_even_once_it_has_a_value() {
    // The tree served for `g` and the leaf served for `g.l` disagree, as when
    // the configuration reloads between two fetches: alone, `g.l` is a
    // constant; in the group, the leaf names itself. `g.l` resolves first
    // (refs bind in sorted order) and has a value; then the group renders its
    // leaf `g.l`, which asks for `g.l` while rendering it — a cycle, not the
    // value it had a moment ago.
    let vars = spec_env();
    let mut f = MapFetch::default();
    f.seed("g.l", Some(CWD), tmpl(json!("{{ 'leaf' }}"))).seed(
        "g",
        Some(CWD),
        tmpl(json!({ "l": "{{ g.l }}" })),
    );
    assert_eq!(
        eval("{{ g.l }}{{ g | length }}", &inputs(&vars), &mut f),
        Err(ResolveError::Config(
            "g.l: virtual field cycle detected: g.l is already being resolved".to_string()
        ))
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

const EXEC_TMPL: &str = "{{ exec(['printf', 'x']) }}";

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
    let src = "prefix {{ exec(['printf', 'x']) }} suffix";
    let mut i = inputs(&env);
    i.allow_exec = true;
    assert_eq!(
        eval(src, &i, &mut f).unwrap(),
        Some(json!("prefix x suffix"))
    );
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
    for src in ["{{ exec('exit 3') }}", "[{{ exec('exit 3') }}]"] {
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
        tmpl(json!("{{ exec(['printf', chain.b]) }}")),
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
fn an_all_empty_cascade_is_absent_alone_and_in_a_group() {
    // Null means absent, never `""`: the daemon's lines read it the same way.
    let env = spec_env();
    let empty = "{{ env.NOPE or comb.nothing.here }}";
    let mut f = MapFetch::default();
    f.seed("k.empty", Some(CWD), tmpl(json!(empty))).seed(
        "k",
        Some(CWD),
        both(
            json!({ "empty": "cached" }),
            json!({ "empty": empty, "set": "{{ 'x' }}" }),
        ),
    );

    assert_eq!(resolve("k.empty", &inputs(&env), &mut f), Ok(None));
    assert_eq!(
        resolve("k", &inputs(&env), &mut f),
        Ok(Some(json!({ "set": "x" }))),
        "an absent leaf is omitted, and nothing cached shows through it"
    );
}

#[test]
fn a_group_with_no_present_leaf_is_absent_at_every_level() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "k",
        Some(CWD),
        tmpl(json!({
            "a": "{{ none }}",
            "sub": { "b": "{{ env.NOPE }}" },
        })),
    )
    .seed(
        "m",
        Some(CWD),
        tmpl(json!({
            "a": "{{ 1 }}",
            "sub": { "b": "{{ none }}", "deeper": { "c": "{{ none }}" } },
        })),
    );

    assert_eq!(resolve("k", &inputs(&env), &mut f), Ok(None));
    assert_eq!(resolve("k.sub", &inputs(&env), &mut f), Ok(None));
    assert_eq!(
        resolve("m", &inputs(&env), &mut f),
        Ok(Some(json!({ "a": 1 }))),
        "an empty subgroup is not a member"
    );
}

#[test]
fn data_the_template_owns_entirely_leaves_nothing_present() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "k",
        Some(CWD),
        both(json!({ "a": "cached" }), json!({ "a": "{{ none }}" })),
    );
    assert_eq!(resolve("k", &inputs(&env), &mut f), Ok(None));
}

#[test]
fn an_empty_object_stored_as_data_is_present() {
    // `{}` written by a put or a Source is a value; a group the client
    // assembles out of absent leaves is not.
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "k",
        Some(CWD),
        both(json!({}), json!({ "a": "{{ none }}" })),
    )
    .seed("plain", Some(CWD), data(json!({})));
    assert_eq!(resolve("k", &inputs(&env), &mut f), Ok(Some(json!({}))));
    assert_eq!(resolve("plain", &inputs(&env), &mut f), Ok(Some(json!({}))));
}

#[test]
fn none_is_absent_and_text_around_it_is_a_string() {
    let env = spec_env();
    let mut f = MapFetch::default();
    assert_eq!(eval("{{ none }}", &inputs(&env), &mut f), Ok(None));
    assert_eq!(
        eval("on {{ none }}", &inputs(&env), &mut f),
        Ok(Some(json!("on ")))
    );
}

#[test]
fn a_number_json_cannot_hold_is_an_error() {
    let env = spec_env();
    let mut f = MapFetch::default();
    match eval("{{ 1 / 0 }}", &inputs(&env), &mut f) {
        Err(ResolveError::Runtime(m)) => assert!(m.contains("inf"), "unexpected message: {m}"),
        other => panic!("expected a runtime error, got {other:?}"),
    }
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
    assert_eq!(
        eval("{{ comb.p.f }}", &i, &mut f).unwrap(),
        Some(json!("field"))
    );
    // Both refs, written in either order: the object supplies `f`.
    assert_eq!(
        eval("{{ comb.p.f or comb.p }}", &i, &mut f).unwrap(),
        Some(json!("whole"))
    );
    assert_eq!(
        eval("{{ comb.p and comb.p.f }}", &i, &mut f).unwrap(),
        Some(json!("whole"))
    );
    // …and the object itself is not damaged by the field binding.
    assert_eq!(
        eval("{{ comb.p.f and comb.p }}", &i, &mut f).unwrap(),
        Some(json!({ "f": "whole" }))
    );

    // A field the object does not carry is still filled in by its own ref.
    f.seed("p", Some(CWD), data(json!({ "other": 1 })));
    assert_eq!(
        eval("{{ comb.p.f or comb.p }}", &i, &mut f).unwrap(),
        Some(json!("field"))
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
        ("[{{ probe.missing.sub }}]", "[]"),
        (r#"{{ probe.missing.sub or "next" }}"#, "next"),
    ] {
        assert_eq!(
            eval(src, &i, &mut f).unwrap(),
            Some(json!(want)),
            "src: {src}"
        );
    }
    assert_eq!(
        eval("{{ probe.missing.sub }}", &i, &mut f).unwrap(),
        None,
        "alone, a chain through a miss is absent"
    );

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
        Some(json!(r#"V|{"f": "V"}"#))
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
        Some(json!("V/FB"))
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
        Some(json!({ "a": 1, "b": "two" }))
    );
    assert_eq!(
        eval("{{ [1, comb.p] }}", &i, &mut f).unwrap(),
        Some(json!([1, { "a": 1, "b": "two" }]))
    );
    assert_eq!(
        eval("{{ {'a': 1} | items }}", &i, &mut f).unwrap(),
        Some(json!([["a", 1]]))
    );
    assert_eq!(
        eval("{{ [1, 2] | reverse }}", &i, &mut f).unwrap(),
        Some(json!([2, 1]))
    );
    assert!(matches!(
        eval("p={{ comb.p }}", &i, &mut f).unwrap(),
        Some(Value::String(_))
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
    assert_eq!(
        eval(src, &inputs(&env), &mut f).unwrap(),
        Some(json!("main*"))
    );

    f.seed("git.dirty", Some(CWD), data(json!(false)));
    assert_eq!(
        eval(src, &inputs(&env), &mut f).unwrap(),
        Some(json!("main"))
    );

    // The env arm inside the referenced template still wins.
    let env = env_of(&[("BRANCH", "feature")]);
    assert_eq!(
        eval(src, &inputs(&env), &mut f).unwrap(),
        Some(json!("feature"))
    );
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
        ResolveError::Fetch(FetchError::Transport(transport_failure("boom.x")))
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
        Some(json!("hi"))
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
        Some(json!(40))
    );
    assert_eq!(
        eval("{{ scanline.stats.cpu + 2 }}", &inputs(&env), &mut f).unwrap(),
        Some(json!(42))
    );
}

#[test]
fn eval_of_tagless_text_is_that_text() {
    let env = spec_env();
    let mut f = spec_fetch();
    assert_eq!(
        eval("hello", &inputs(&env), &mut f).unwrap(),
        Some(json!("hello"))
    );
    // An empty source is a template of no tags — `-f ''` asks for exactly
    // this — and whitespace-only is literal text that survives.
    assert_eq!(eval("", &inputs(&env), &mut f).unwrap(), Some(json!("")));
    assert_eq!(
        eval("   ", &inputs(&env), &mut f).unwrap(),
        Some(json!("   "))
    );
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

// ── 16. Refs used other than by attribute ─────────────────────────────────────
//
// A ref subscripted, filtered or iterated names the node at the end of its
// literal attribute chain, read whole. `comb` alone is every provider, so
// `comb[...]` reads the member the render asks for, when it asks.

fn render_eval(src: &str, f: &mut MapFetch, env: &HashMap<String, String>) -> Option<Value> {
    eval(src, &inputs(env), f).unwrap_or_else(|e| panic!("{src}: {e}"))
}

#[test]
fn a_bracketed_comb_head_reads_that_node_whole() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "git:cache",
        Some(CWD),
        data(json!({"branch": {"age_ms": 7, "source": "source(head)"}})),
    );
    assert_eq!(
        render_eval("{{ comb['git:cache'].branch.age_ms }}", &mut f, &env),
        Some(json!(7))
    );
    assert_eq!(f.keys(), vec!["git:cache"]);
}

#[test]
fn a_computed_comb_name_reads_the_provider_it_names() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed("sel.which", Some(CWD), data(json!("qa"))).seed(
        "qa",
        Some(CWD),
        data(json!({"v": "a1"})),
    );
    assert_eq!(
        render_eval("{{ comb[comb.sel.which].v }}", &mut f, &env),
        Some(json!("a1"))
    );
    assert_eq!(f.keys(), vec!["sel.which", "qa"]);
}

#[test]
fn a_comb_name_nothing_answers_is_undefined_so_default_fires() {
    let env = spec_env();
    let mut f = MapFetch::default();
    assert_eq!(
        render_eval("{{ comb['nope'] | default('fb') }}", &mut f, &env),
        Some(json!("fb"))
    );
    assert_eq!(
        render_eval("{{ comb['a.b'] is defined }}", &mut f, &env),
        Some(json!(false)),
        "a name that is not a provider head names nothing"
    );
}

#[test]
fn a_literal_comb_field_stays_precise_beside_a_computed_name() {
    // `comb.git.branch` reads `git.branch` alone, never the whole of `git`,
    // even in a render that also reads `comb[...]` — and a computed member
    // of that literal head is read alone too.
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed("git.branch", Some(CWD), data(json!("main")))
        .seed("git.dirty", Some(CWD), data(json!(true)))
        .seed("qa", Some(CWD), data(json!({"v": "a1"})));
    assert_eq!(
        render_eval(
            "{{ comb.git.branch ~ '/' ~ comb['qa'].v ~ '/' ~ comb['git']['dirty'] }}",
            &mut f,
            &env
        ),
        Some(json!("main/a1/true"))
    );
    let keys = f.keys();
    assert!(
        !keys.contains(&"git"),
        "the whole of git was never read: {keys:?}"
    );
    assert!(keys.contains(&"git.branch") && keys.contains(&"git.dirty"));
}

#[test]
fn a_subscript_after_a_literal_provider_reads_that_provider_whole() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "mise",
        Some(CWD),
        data(json!({"cargo:x": "1.0", "go:golang.org/x": "0.16"})),
    );
    assert_eq!(
        render_eval(
            "{{ comb.mise['cargo:x'] ~ ' ' ~ comb.mise['go:golang.org/x'] }}",
            &mut f,
            &env
        ),
        Some(json!("1.0 0.16"))
    );
    assert_eq!(f.keys(), vec!["mise"]);
}

#[test]
fn a_bare_provider_subscripted_is_that_provider_resolved() {
    let env = spec_env();
    let mut f = spec_fetch();
    assert_eq!(
        render_eval("{{ scanline['widgets']['foo'] }}", &mut f, &env),
        Some(json!("hi"))
    );
    assert_eq!(
        render_eval("{{ nope['x'] | default('fb') }}", &mut f, &env),
        Some(json!("fb"))
    );
}

#[test]
fn the_caller_environment_is_bound_whole() {
    let vars = env_of(&[("A", "1"), ("B", "2")]);
    let mut f = MapFetch::default();
    assert_eq!(
        render_eval("{{ env[('A')] ~ env['B'] }}", &mut f, &vars),
        Some(json!("12"))
    );
    assert_eq!(
        render_eval(
            "{% for k, v in env | items %}{{ k }}={{ v }};{% endfor %}",
            &mut f,
            &vars
        ),
        Some(json!("A=1;B=2;"))
    );
    assert_eq!(
        render_eval("{{ env['NOPE'] | default('fb') }}", &mut f, &vars),
        Some(json!("fb"))
    );
    assert!(f.log.is_empty(), "the environment asks the daemon nothing");
}

/// Every way a template reads the caller's environment, in a render with an
/// eager context, one with a lazy `comb[...]`, a leaf of a group, and a
/// template another one reads — each sees the whole environment, and an
/// unset variable is undefined in all of them.
#[test]
fn every_form_of_env_reads_the_caller_environment() {
    let vars = env_of(&[("A", "1"), ("B", "2")]);
    let forms = [
        ("{{ env.A }}", "1"),
        ("{{ env['A'] }}", "1"),
        ("{% set n = 'B' %}{{ env[n] }}", "2"),
        (
            "{% for k, v in env | items %}{{ k }}={{ v }};{% endfor %}",
            "A=1;B=2;",
        ),
        ("{% for k in env %}{{ k }},{% endfor %}", "A,B,"),
        ("{{ env | length }}", "2"),
        ("{{ env.NOPE is defined }}", "false"),
        ("{{ env['NOPE'] is undefined }}", "true"),
        ("{{ env.NOPE | default('fb') }}", "fb"),
        ("[{{ env.NOPE }}]", "[]"),
    ];
    let lazy = "{{ comb['nope'] | default('') }}";
    for (src, want) in forms {
        let mut f = MapFetch::default();
        assert_eq!(
            render_eval(&format!("{src}{lazy}"), &mut f, &vars),
            Some(json!(want)),
            "with a lazy comb: {src}"
        );
        // A string result, whichever form: one tag of text renders the same.
        let as_text = |v: Option<Value>| v.map(|v| v.as_str().map_or(v.to_string(), String::from));
        f.seed("t.leaf", Some(CWD), tmpl(json!(src)))
            .seed("t", Some(CWD), tmpl(json!({ "leaf": src })))
            .seed("t.outer", Some(CWD), tmpl(json!("{{ t.leaf }}")));
        let i = inputs(&vars);
        assert_eq!(
            as_text(eval(src, &i, &mut f).unwrap()),
            Some(want.to_string()),
            "{src}"
        );
        assert_eq!(
            as_text(resolve("t", &i, &mut f).unwrap().map(|g| g["leaf"].clone())),
            Some(want.to_string()),
            "a group's leaf: {src}"
        );
        assert_eq!(
            as_text(resolve("t.outer", &i, &mut f).unwrap()),
            Some(want.to_string()),
            "read by another template: {src}"
        );
    }
    // Whole, the environment is an object of strings; an unset variable is
    // absent, not `""`.
    let mut f = MapFetch::default();
    assert_eq!(
        render_eval("{{ env }}", &mut f, &vars),
        Some(json!({ "A": "1", "B": "2" }))
    );
    assert_eq!(render_eval("{{ env.NOPE }}", &mut f, &vars), None);
    assert!(f.log.is_empty(), "the environment asks the daemon nothing");
}

#[test]
fn what_a_lazy_read_fetched_is_a_dependency() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed("k.t", Some(CWD), tmpl(json!("{{ comb['qa'].v }}")))
        .seed("qa", Some(CWD), data(json!({"v": "a1"})));
    let Traced { value, deps, .. } = resolve_traced("k.t", &inputs(&env), &mut f).unwrap();
    assert_eq!(value, Some(json!("a1")));
    assert_eq!(
        deps,
        vec![
            ("k.t".to_string(), Some(CWD.to_string())),
            ("qa".to_string(), Some(CWD.to_string())),
        ]
    );
}

// ── a rendered value is as old as its oldest input ───────────────────────────

/// Cached data `value`, `age_ms` old.
fn aged(value: Value, age_ms: u128) -> Got {
    Got {
        data: Some(value),
        age_ms: Some(age_ms),
        stale: Some(false),
        ..Got::default()
    }
}

#[test]
fn a_rendered_value_is_as_old_as_the_oldest_cached_input_it_used() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "k.t",
        Some(CWD),
        tmpl(json!("{{ comb.a.v }}-{{ b.v }}-{{ comb['c'].v }}")),
    )
    .seed("a.v", Some(CWD), aged(json!("x"), 5_000))
    .seed("b.v", Some(CWD), aged(json!("y"), 90_000))
    .seed("c", Some(CWD), aged(json!({"v": "z"}), 40_000));
    let traced = resolve_traced("k.t", &inputs(&env), &mut f).unwrap();
    assert_eq!(traced.value, Some(json!("x-y-z")));
    assert_eq!(traced.oldest_input_ms, Some(90_000));
}

#[test]
fn a_rendered_value_with_no_cached_input_has_no_age() {
    let env = spec_env();
    let mut f = MapFetch::default();
    // The template's own record is not an input: data shadowed by the
    // template at the same address is never used.
    f.seed(
        "k.t",
        Some(CWD),
        Got {
            template: Some(json!("{{ env.SCANLINE_FOO }}")),
            ..aged(json!("shadowed"), 70_000)
        },
    );
    let traced = resolve_traced("k.t", &inputs(&env), &mut f).unwrap();
    assert_eq!(traced.value, Some(json!("hi")));
    assert_eq!(traced.oldest_input_ms, None);
}

#[test]
fn plain_data_is_as_old_as_itself() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed("p.v", Some(CWD), aged(json!(1), 12_000));
    let traced = resolve_traced("p.v", &inputs(&env), &mut f).unwrap();
    assert_eq!(traced.oldest_input_ms, Some(12_000));
}

#[test]
fn a_transport_failure_in_a_lazy_read_fails_the_resolution() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed(
        "k.t",
        Some(CWD),
        tmpl(json!("{{ comb['down'].v or 'fb' }}")),
    );
    f.fails.push("down".to_string());
    match resolve("k.t", &inputs(&env), &mut f) {
        Err(ResolveError::Fetch(FetchError::Transport(e))) => {
            assert!(e.to_string().contains("down"), "{e}")
        }
        other => panic!("expected the transport failure, got {other:?}"),
    }
}

#[test]
fn a_lazy_comb_node_returned_whole_is_its_data() {
    let env = spec_env();
    let mut f = MapFetch::default();
    f.seed("qa", Some(CWD), data(json!({"v": "a1"})));
    assert_eq!(
        render_eval("{{ comb['qa'] }}", &mut f, &env),
        Some(json!({"v": "a1"}))
    );
    assert_eq!(render_eval("{{ comb['nope'] }}", &mut f, &env), None);
}

// ── a template runs in the directory the resolution addresses ────────────────

/// `read` (like `lup` and `exec`) runs in the directory the resolution
/// addresses: an explicit path wins over the caller's directory, and the
/// global entry is `$HOME`.
#[test]
fn a_templates_files_are_read_in_the_addressed_directory() {
    let addressed = tempfile::tempdir().unwrap();
    let caller = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    for (dir, text) in [
        (&addressed, "addressed"),
        (&caller, "caller"),
        (&home, "home"),
    ] {
        std::fs::write(dir.path().join("marker"), text).unwrap();
    }
    let path = |d: &tempfile::TempDir| d.path().to_str().unwrap().to_string();
    let env = env_of(&[("HOME", &path(&home))]);
    let read = |slot: Slot| {
        let mut f = MapFetch::default();
        let at = match &slot {
            Slot::Path(p) => Some(p.clone()),
            Slot::Cwd => Some(path(&caller)),
            Slot::Global => None,
        };
        f.seed("t.m", at.as_deref(), tmpl(json!("{{ 'marker' | read }}")));
        let caller_dir = path(&caller);
        let inputs = Inputs {
            cwd: &caller_dir,
            env: &env,
            slot,
            force: false,
            wait: libbeachcomber::Wait::Budget,
            allow_exec: false,
            now: std::time::SystemTime::now(),
        };
        resolve("t.m", &inputs, &mut f).unwrap()
    };

    assert_eq!(read(Slot::Path(path(&addressed))), Some(json!("addressed")));
    assert_eq!(read(Slot::Cwd), Some(json!("caller")));
    assert_eq!(read(Slot::Global), Some(json!("home")));
}

// ── 17. Each node renders once per resolution ────────────────────────────────

/// A leaf whose every render appends one `x` to the file `$COUNT` names, and
/// whose value is `v`: the file's length is how many times it rendered.
const COUNTED: &str = r#"{{ exec('printf x >> "$COUNT"; printf v') }}"#;

/// `k.shared` read from five places in one resolution: directly, through two
/// other templates, and through both leaves of a group read whole.
fn shared_fetch() -> MapFetch {
    let group = json!({ "one": "{{ k.shared }}", "two": "{{ k.shared }}" });
    let mut f = MapFetch::default();
    f.seed("k.shared", Some(CWD), tmpl(json!(COUNTED)))
        .seed("k.a", Some(CWD), tmpl(json!("{{ k.shared }}-a")))
        .seed("k.b", Some(CWD), tmpl(json!("{{ k.shared }}-b")))
        .seed("g", Some(CWD), tmpl(group))
        .seed(
            "k.top",
            Some(CWD),
            tmpl(json!(
                "{{ k.shared }} {{ k.a }} {{ k.b }} {% for _, v in g | items %}{{ v }}{% endfor %}"
            )),
        );
    f
}

#[test]
fn a_node_read_from_many_places_renders_once_per_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let count = dir.path().join("count");
    let vars = env_of(&[("COUNT", count.to_str().unwrap())]);
    let renders = || std::fs::read_to_string(&count).unwrap_or_default().len();
    let mut i = inputs(&vars);
    i.allow_exec = true;

    let mut f = shared_fetch();
    assert_eq!(
        resolve("k.top", &i, &mut f),
        Ok(Some(json!("v v-a v-b vv")))
    );
    assert_eq!(renders(), 1, "five reads, one render");

    // The memo is the resolution's own: the next one renders afresh.
    assert_eq!(
        resolve("k.top", &i, &mut f),
        Ok(Some(json!("v v-a v-b vv")))
    );
    assert_eq!(renders(), 2);
}

#[test]
fn a_node_that_failed_to_render_is_not_rendered_again() {
    // Both leaves of the group read `k.fail`, whose command fails: each leaf
    // is omitted, and the command ran once.
    let dir = tempfile::tempdir().unwrap();
    let count = dir.path().join("count");
    let vars = env_of(&[("COUNT", count.to_str().unwrap())]);
    let mut i = inputs(&vars);
    i.allow_exec = true;
    let mut f = MapFetch::default();
    f.seed(
        "k.fail",
        Some(CWD),
        tmpl(json!(r#"{{ exec('printf x >> "$COUNT"; exit 3') }}"#)),
    )
    .seed(
        "g",
        Some(CWD),
        tmpl(json!({ "a": "{{ k.fail }}", "b": "{{ k.fail }}", "c": "ok" })),
    );
    assert_eq!(resolve("g", &i, &mut f), Ok(Some(json!({ "c": "ok" }))));
    assert_eq!(std::fs::read_to_string(&count).unwrap(), "x");
}

#[test]
fn a_node_read_again_reports_the_same_deps_and_age() {
    // `k.shared` holds the oldest input, `d.old`, and is read twice: through
    // `k.a`, then by `k.top` itself. The dependency list is every key once in
    // first-fetch order, and the age is `d.old`'s, as when each read rendered.
    let vars = spec_env();
    let mut f = MapFetch::default();
    f.seed("k.top", Some(CWD), tmpl(json!("{{ k.a }}|{{ k.shared }}")))
        .seed(
            "k.a",
            Some(CWD),
            tmpl(json!("{{ k.shared }}{{ comb.e.x }}")),
        )
        .seed(
            "k.shared",
            Some(CWD),
            tmpl(json!("{{ comb.d.old }}{{ d.new }}")),
        )
        .seed("d.old", Some(CWD), aged(json!("o"), 90_000))
        .seed("d.new", Some(CWD), aged(json!("n"), 5_000))
        .seed("e.x", Some(CWD), aged(json!("e"), 1_000));

    let traced = resolve_traced("k.top", &inputs(&vars), &mut f).unwrap();
    assert_eq!(traced.value, Some(json!("one|on")));
    let at = |k: &str| (k.to_string(), Some(CWD.to_string()));
    assert_eq!(
        traced.deps,
        vec![
            at("k.top"),
            at("k.a"),
            at("e.x"),
            at("k.shared"),
            at("d.old"),
            at("d.new"),
        ]
    );
    assert_eq!(traced.oldest_input_ms, Some(90_000));
    assert_eq!(
        f.keys(),
        vec!["k.top", "k.a", "e.x", "k.shared", "d.old", "d.new"],
        "each key fetched once"
    );
}

// ── Caching a template ────────────────────────────────────────────────────────

/// The hash `cache()` computes over these inputs and no file.
fn hash_of(inputs: &[&str]) -> u64 {
    let inputs: Vec<minijinja::Value> = inputs.iter().map(|i| minijinja::Value::from(*i)).collect();
    libbeachcomber::library::memo::memo_hash(&inputs, &[]).unwrap()
}

/// A memo half: live under `hash`, `age_ms` old, holding `data`.
fn memo(hash: u64, data: Option<Value>, age_ms: u64) -> Value {
    let mut half = json!({
        "hash": libbeachcomber::validity::hex(hash),
        "stale": false,
        "age_ms": age_ms,
    });
    if let Some(data) = data {
        half["data"] = data;
    }
    half
}

/// A template leaf with its memo half beside it.
fn cached(t: &str, half: Option<Value>) -> Got {
    Got {
        template: Some(json!(t)),
        memo: half,
        ..Got::default()
    }
}

fn exec_inputs(env: &HashMap<String, String>) -> Inputs<'_> {
    Inputs {
        allow_exec: true,
        ..inputs(env)
    }
}

/// A leaf that misses renders on, and its final output — what follows the
/// `exec()` included — is queued under the hash of its inputs, at the
/// resolution's directory, with what `watch()` and `ttl()` recorded. The
/// library's `resolve` writes it before returning; `resolve_traced` leaves
/// it to the caller.
#[test]
fn a_miss_renders_and_queues_a_write_with_the_leafs_final_output() {
    let env = env_of(&[]);
    let t = "{{ 'echo x' | watch('/nonexistent-comb/f') | ttl('10m') | cache() | exec() | upper }}";
    let mut f = MapFetch::default();
    f.seed("p.f", Some(CWD), cached(t, None));

    let traced = resolve_traced("p.f", &exec_inputs(&env), &mut f).unwrap();
    assert_eq!(traced.value, Some(json!("X")));
    assert!(!traced.memo_hit);
    let paths = [std::path::PathBuf::from("/nonexistent-comb/f")];
    let hash =
        libbeachcomber::library::memo::memo_hash(&[minijinja::Value::from("echo x")], &paths)
            .unwrap();
    let want = MemoWrite {
        key: "p.f".into(),
        path: Some(CWD.into()),
        hash,
        data: Some(json!("X")),
        watch: paths.to_vec(),
        ttl: Some(std::time::Duration::from_secs(600)),
    };
    assert_eq!(traced.memos, std::slice::from_ref(&want));
    assert!(f.writes.is_empty(), "traced: the caller writes");

    assert_eq!(
        resolve("p.f", &exec_inputs(&env), &mut f),
        Ok(Some(json!("X")))
    );
    assert_eq!(f.writes, [want], "resolve writes before it returns");
}

/// A hit is the memo, and nothing after `cache()` runs: `false` would fail.
#[test]
fn a_hit_skips_exec() {
    let env = env_of(&[]);
    let hash = hash_of(&["false"]);
    let mut f = MapFetch::default();
    f.seed(
        "p.f",
        Some(CWD),
        cached(
            "{{ 'false' | cache() | exec() }}",
            Some(memo(hash, Some(json!("prod")), 5)),
        ),
    );
    let traced = resolve_traced("p.f", &exec_inputs(&env), &mut f).unwrap();
    assert_eq!(traced.value, Some(json!("prod")));
    assert!(traced.memo_hit);
    assert!(traced.memos.is_empty(), "a hit writes nothing");
}

#[test]
fn a_hit_of_absence_is_absent() {
    let env = env_of(&[]);
    let mut f = MapFetch::default();
    f.seed(
        "p.f",
        Some(CWD),
        cached(
            "{{ 'false' | cache() | exec() }}",
            Some(memo(hash_of(&["false"]), None, 5)),
        ),
    );
    assert_eq!(resolve("p.f", &exec_inputs(&env), &mut f), Ok(None));
    assert!(f.writes.is_empty());
}

/// A memo past its ttl, one under another hash, and one whose hash was
/// dropped are each a miss: the leaf renders and is stored again.
#[test]
fn a_stale_memo_is_a_miss() {
    let env = env_of(&[("V", "now")]);
    let hash = hash_of(&["now"]);
    let mut half = memo(hash, Some(json!("then")), 5);
    half["stale"] = json!(true);
    let mut f = MapFetch::default();
    f.seed(
        "p.f",
        Some(CWD),
        cached("{{ env.V | cache() | upper }}", Some(half)),
    );
    assert_eq!(
        resolve("p.f", &inputs(&env), &mut f),
        Ok(Some(json!("NOW")))
    );
    assert_eq!(f.writes.len(), 1);
    assert_eq!(f.writes[0].hash, hash);
}

#[test]
fn a_differing_hash_is_a_miss() {
    let env = env_of(&[("V", "now")]);
    for half in [
        memo(hash_of(&["then"]), Some(json!("THEN")), 5),
        json!({"stale": false, "age_ms": 5, "data": "THEN"}),
    ] {
        let mut f = MapFetch::default();
        f.seed(
            "p.f",
            Some(CWD),
            cached("{{ env.V | cache() | upper }}", Some(half)),
        );
        assert_eq!(
            resolve("p.f", &inputs(&env), &mut f),
            Ok(Some(json!("NOW")))
        );
        assert_eq!(f.writes.len(), 1, "stored again");
    }
}

/// `--force` renders past a live memo, and stores what it rendered.
#[test]
fn force_renders_past_the_memo() {
    let env = env_of(&[("V", "now")]);
    let mut f = MapFetch::default();
    f.seed(
        "p.f",
        Some(CWD),
        cached(
            "{{ env.V | cache() | upper }}",
            Some(memo(hash_of(&["now"]), Some(json!("OLD")), 5)),
        ),
    );
    let forced = Inputs {
        force: true,
        ..inputs(&env)
    };
    assert_eq!(resolve("p.f", &forced, &mut f), Ok(Some(json!("NOW"))));
    assert_eq!(f.writes.len(), 1);
}

/// In a group each leaf reads its own memo, at its place in the half: one
/// leaf's hit leaves its siblings to render, and only a leaf that missed is
/// stored.
#[test]
fn a_hit_inside_a_group_keeps_its_siblings() {
    let env = env_of(&[("V", "v")]);
    let tree = json!({
        "a": "{{ env.V | cache() | upper }}",
        "sub": {"b": "{{ env.V | cache() ~ '!' }}"},
        "c": "{{ env.V }}",
    });
    let mut f = MapFetch::default();
    f.seed(
        "p",
        Some(CWD),
        Got {
            template: Some(tree),
            memo: Some(json!({"a": memo(hash_of(&["v"]), Some(json!("MEMO")), 5)})),
            ..Got::default()
        },
    );
    assert_eq!(
        resolve("p", &inputs(&env), &mut f),
        Ok(Some(json!({"a": "MEMO", "sub": {"b": "v!"}, "c": "v"})))
    );
    let keys: Vec<&str> = f.writes.iter().map(|w| w.key.as_str()).collect();
    assert_eq!(keys, ["p.sub.b"]);
    assert_eq!(f.writes[0].data, Some(json!("v!")));
}

/// A leaf that fails stores nothing: its memo stays as it was.
#[test]
fn a_failed_leaf_writes_nothing() {
    let env = env_of(&[]);
    let mut f = MapFetch::default();
    f.seed(
        "p.f",
        Some(CWD),
        cached("{{ 'false' | cache() | exec() }}", None),
    );
    assert!(matches!(
        resolve("p.f", &exec_inputs(&env), &mut f),
        Err(ResolveError::Runtime(_))
    ));
    assert!(f.writes.is_empty());
}

/// A value served from a memo is as old as the memo, as one read from any
/// stored input is.
#[test]
fn a_hit_counts_the_memos_age() {
    let env = env_of(&[("V", "v")]);
    let mut f = MapFetch::default();
    f.seed(
        "p.f",
        Some(CWD),
        cached(
            "{{ env.V | cache() | upper }}",
            Some(memo(hash_of(&["v"]), Some(json!("V")), 42_000)),
        ),
    );
    let traced = resolve_traced("p.f", &inputs(&env), &mut f).unwrap();
    assert_eq!(traced.oldest_input_ms, Some(42_000));
}

/// `comb eval` and a `-f` format are stored nowhere: their own `cache()`
/// passes through, records nothing and writes nothing, however often it runs.
/// A field they read whose template caches is served from its memo.
#[test]
fn eval_and_a_format_never_write() {
    let env = env_of(&[("V", "v")]);
    let mut f = MapFetch::default();
    f.seed(
        "p.f",
        Some(CWD),
        cached(
            "{{ env.V | cache() | upper }}",
            Some(memo(hash_of(&["v"]), Some(json!("MEMO")), 5)),
        ),
    );
    for _ in 0..2 {
        assert_eq!(
            eval(
                "{{ env.V | watch('/f') | ttl('1h') | cache() | upper }}",
                &inputs(&env),
                &mut f
            ),
            Ok(Some(json!("V")))
        );
        let locals = serde_json::Map::from_iter([("value".to_string(), json!("x"))]);
        assert_eq!(
            eval_with(
                "{{ value | cache() }}",
                "-f",
                &inputs(&env),
                &mut f,
                &locals
            ),
            Ok(Some(json!("x")))
        );
    }
    assert_eq!(
        eval("{{ p.f }}", &inputs(&env), &mut f),
        Ok(Some(json!("MEMO"))),
        "a field caches as its template says"
    );
    assert!(f.writes.is_empty());
}

/// A leaf whose render reads `comb[...]` renders on a thread of its own; it
/// still caches under its own address, and a cached leaf it reads caches
/// under that leaf's.
#[test]
fn a_lazy_comb_render_keeps_its_own_frame() {
    let env = env_of(&[("N", "d")]);
    let mut f = MapFetch::default();
    f.seed(
        "p.a",
        Some(CWD),
        cached("{{ (comb[env.N].x ~ p.b) | cache() | upper }}", None),
    )
    .seed(
        "p.b",
        Some(CWD),
        cached("{{ env.N | cache() ~ '-b' }}", None),
    )
    .seed("d", Some(CWD), data(json!({"x": "x"})));
    assert_eq!(
        resolve("p.a", &inputs(&env), &mut f),
        Ok(Some(json!("XD-B")))
    );
    let written: Vec<(&str, u64, Option<&Value>)> = f
        .writes
        .iter()
        .map(|w| (w.key.as_str(), w.hash, w.data.as_ref()))
        .collect();
    assert_eq!(
        written,
        [
            ("p.b", hash_of(&["d"]), Some(&json!("d-b"))),
            ("p.a", hash_of(&["xd-b"]), Some(&json!("XD-B"))),
        ]
    );
}

/// A memo stored by a client that may run `exec()` serves one that may not:
/// the hit ends the render before the `exec()` would be refused.
#[test]
fn a_hit_on_a_client_without_exec() {
    let env = env_of(&[]);
    let mut f = MapFetch::default();
    f.seed(
        "p.f",
        Some(CWD),
        cached(
            "{{ 'kubectl x' | cache() | exec() }}",
            Some(memo(hash_of(&["kubectl x"]), Some(json!("prod")), 5)),
        ),
    );
    assert_eq!(
        resolve("p.f", &inputs(&env), &mut f),
        Ok(Some(json!("prod")))
    );
    f.entries.clear();
    f.seed(
        "p.f",
        Some(CWD),
        cached("{{ 'kubectl x' | cache() | exec() }}", None),
    );
    assert!(matches!(
        resolve("p.f", &inputs(&env), &mut f),
        Err(ResolveError::Runtime(m)) if m.contains("exec() is disabled in this context")
    ));
}

/// An address past a cached leaf walks into its memo, and a miss there stores
/// the leaf's whole value under the leaf's address.
#[test]
fn walk_into_a_memo_hit() {
    let env = env_of(&[]);
    let t = "{{ '{\"cpu\": 40}' | cache() | fromjson }}";
    let hash = hash_of(&["{\"cpu\": 40}"]);
    let mut f = MapFetch::default();
    f.seed(
        "p.stats.cpu",
        Some(CWD),
        Got {
            memo: Some(memo(hash, Some(json!({"cpu": 7})), 5)),
            ..tmpl_rest(json!(t), "cpu")
        },
    );
    assert_eq!(
        resolve("p.stats.cpu", &inputs(&env), &mut f),
        Ok(Some(json!(7)))
    );
    f.seed("p.stats.cpu", Some(CWD), tmpl_rest(json!(t), "cpu"));
    let traced = resolve_traced("p.stats.cpu", &inputs(&env), &mut f).unwrap();
    assert_eq!(traced.value, Some(json!(40)));
    assert_eq!(traced.memos.len(), 1);
    assert_eq!(traced.memos[0].key, "p.stats");
    assert_eq!(traced.memos[0].data, Some(json!({"cpu": 40})));
}

/// A reader that waits nothing takes a stale memo rather than render: one
/// under its hash, and one whose hash its expiry dropped. It is as old as the
/// memo, and nothing is written.
#[test]
fn a_zero_wait_takes_a_stale_memo() {
    let env = env_of(&[]);
    let zero = Inputs {
        wait: libbeachcomber::Wait::For(std::time::Duration::ZERO),
        ..exec_inputs(&env)
    };
    let mut equal = memo(hash_of(&["false"]), Some(json!("old")), 90_000);
    equal["stale"] = json!(true);
    let dropped = json!({"stale": true, "age_ms": 90_000, "data": "old"});
    for half in [equal, dropped] {
        let mut f = MapFetch::default();
        f.seed(
            "p.f",
            Some(CWD),
            cached("{{ 'false' | cache() | exec() }}", Some(half)),
        );
        let traced = resolve_traced("p.f", &zero, &mut f).unwrap();
        assert_eq!(traced.value, Some(json!("old")), "`false` never ran");
        assert!(traced.memo_hit);
        assert_eq!(traced.oldest_input_ms, Some(90_000));
        assert!(traced.memos.is_empty());
    }
}

/// Every other reader renders a stale memo, and stores what it rendered; so
/// does one that waits nothing when the memo within its ttl lost its hash
/// (an invalidation, not an expiry).
#[test]
fn a_budget_wait_renders_a_stale_memo() {
    let env = env_of(&[("V", "now")]);
    let mut stale = memo(hash_of(&["now"]), Some(json!("OLD")), 5);
    stale["stale"] = json!(true);
    let ms = |n| libbeachcomber::Wait::For(std::time::Duration::from_millis(n));
    for wait in [
        libbeachcomber::Wait::Budget,
        ms(50),
        libbeachcomber::Wait::Forever,
    ] {
        let mut f = MapFetch::default();
        f.seed(
            "p.f",
            Some(CWD),
            cached("{{ env.V | cache() | upper }}", Some(stale.clone())),
        );
        let reader = Inputs {
            wait,
            ..inputs(&env)
        };
        assert_eq!(
            resolve("p.f", &reader, &mut f),
            Ok(Some(json!("NOW"))),
            "{wait:?}"
        );
        assert_eq!(f.writes.len(), 1, "{wait:?}");
    }
    let mut f = MapFetch::default();
    f.seed(
        "p.f",
        Some(CWD),
        cached(
            "{{ env.V | cache() | upper }}",
            Some(json!({"stale": false, "age_ms": 5, "data": "OLD"})),
        ),
    );
    let zero = Inputs {
        wait: ms(0),
        ..inputs(&env)
    };
    assert_eq!(resolve("p.f", &zero, &mut f), Ok(Some(json!("NOW"))));
}
