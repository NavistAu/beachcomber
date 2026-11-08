//! Absent in, absent out, for minijinja's own filters.
//!
//! Given `none` or undefined, every filter of the library gives undefined, so
//! `| default(x)` fires without a second argument and `~` joins it as nothing.
//! The library's own items do that themselves; [`guard_builtins`] re-registers
//! minijinja's through the same rule.
//!
//! The filters in [`EXEMPT`] exist to handle absence or to render it, and are
//! left as minijinja has them.

use super::{UNDEFINED, is_absent};
use minijinja::value::{Rest, Value as MjValue, ValueOrKwargs};
use minijinja::{Environment, Error, State, filters};
use std::sync::LazyLock;

/// The builtins that take an absent subject as they are: they supply a default,
/// test truth, or show the value.
pub const EXEMPT: &[&str] = &[
    "default", "d", "bool", "tojson", "pprint", "safe", "escape", "e",
];

/// An untouched environment, for the builtins minijinja registers as a
/// crate-private variant of the public function (`replace` with `count=`,
/// `length` and `items` checked, ...): there they are reached as registered.
static BUILTINS: LazyLock<Environment<'static>> = LazyLock::new(Environment::new);

/// Register `name` to call `inner`, the builtin, unless its subject is absent.
fn guard(
    jinja: &mut Environment<'_>,
    name: &'static str,
    inner: impl Fn(&mut State, &[MjValue]) -> Result<MjValue, Error> + Send + Sync + 'static,
) {
    jinja.add_filter(name, move |state: &mut State, args: Rest<ValueOrKwargs>| {
        let args = args.into_values();
        match args.first() {
            Some(subject) if is_absent(subject) => Ok(UNDEFINED),
            _ => inner(state, &args),
        }
    });
}

macro_rules! guard {
    ($jinja:expr, $($name:literal => $f:path),+ $(,)?) => {$(
        let f = MjValue::from_function($f);
        guard($jinja, $name, move |state, args| f.call(state, args));
    )+};
    ($jinja:expr, builtin $($name:literal),+ $(,)?) => {$(
        guard($jinja, $name, |_, args| BUILTINS.empty_state().apply_filter($name, args));
    )+};
}

/// Re-register minijinja's builtin filters (but those in [`EXEMPT`]) so an
/// absent subject gives undefined.
pub(crate) fn guard_builtins(jinja: &mut Environment<'_>) {
    // Registered as private variants. Their state is read only for fuel (not
    // enabled) and an undefined subject (the guard's), so another
    // environment's state gives the same answer.
    guard!(jinja, builtin
        "replace", "length", "count", "dictsort", "items", "reverse",
        "first", "last", "indent", "groupby",
    );
    // Registered as these public functions, called with the render's state:
    // `map` and `select` reach its filters and tests by name.
    guard!(jinja,
        "upper" => filters::upper,
        "lower" => filters::lower,
        "title" => filters::title,
        "capitalize" => filters::capitalize,
        "trim" => filters::trim,
        "join" => filters::join,
        "split" => filters::split,
        "lines" => filters::lines,
        "abs" => filters::abs,
        "int" => filters::int,
        "float" => filters::float,
        "sum" => filters::sum,
        "attr" => filters::attr,
        "round" => filters::round,
        "min" => filters::min,
        "max" => filters::max,
        "sort" => filters::sort,
        "list" => filters::list,
        "string" => filters::string,
        "slice" => filters::slice,
        "batch" => filters::batch,
        "select" => filters::select,
        "selectattr" => filters::selectattr,
        "reject" => filters::reject,
        "rejectattr" => filters::rejectattr,
        "map" => filters::map,
        "unique" => filters::unique,
        "chain" => filters::chain,
        "zip" => filters::zip,
        "format" => filters::format,
        "urlencode" => filters::urlencode,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::build_pure_engine_env;

    /// Every filter the environment has, from its own debug listing, so a
    /// builtin a minijinja upgrade adds cannot slip past.
    fn filter_names(jinja: &Environment<'_>) -> Vec<String> {
        let dump = format!("{jinja:#?}");
        let start = dump.find("filters: [").expect("a filters listing") + "filters: [".len();
        let body = &dump[start..];
        let body = &body[..body.find(']').expect("the listing closes")];
        body.split(',')
            .filter_map(|n| n.trim().strip_prefix('"')?.strip_suffix('"'))
            .map(str::to_string)
            .collect()
    }

    /// The arguments a filter needs besides its subject.
    fn arguments(name: &str) -> Vec<MjValue> {
        match name {
            "truncate" | "pad" => vec![MjValue::from(3)],
            "fg" | "bg" => vec![MjValue::from("#ff0000")],
            "pluck" => vec![MjValue::from("a")],
            "map" => vec![MjValue::from("upper")],
            "regex_search" => vec![MjValue::from("a")],
            "regex_replace" => vec![MjValue::from("a"), MjValue::from("b")],
            "relpath" => vec![MjValue::from("/a")],
            "hash" => vec![MjValue::from("md5")],
            "ramp" => vec![MjValue::from(vec![50]), MjValue::from(vec!["lo", "hi"])],
            "pick" | "omit" => vec![MjValue::from("a")],
            "clamp" => vec![MjValue::from(0), MjValue::from(1)],
            _ => vec![],
        }
    }

    #[test]
    fn every_filter_returns_undefined_for_an_absent_subject() {
        let jinja = build_pure_engine_env(Default::default());
        let mut state = jinja.empty_state();
        let names = filter_names(&jinja);
        assert!(names.len() > 40, "the listing was not read: {names:?}");
        for exempt in EXEMPT {
            assert!(names.iter().any(|n| n == exempt), "no filter {exempt}");
        }
        for name in names.iter().filter(|n| !EXEMPT.contains(&n.as_str())) {
            for subject in [MjValue::UNDEFINED, MjValue::from(())] {
                let mut args = vec![subject];
                args.extend(arguments(name));
                let got = state
                    .apply_filter(name, &args)
                    .unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(
                    got.is_undefined(),
                    "{name} gave {got:?} for an absent subject"
                );
            }
        }
    }

    #[test]
    fn the_exempt_filters_handle_an_absent_subject_themselves() {
        let jinja = build_pure_engine_env(Default::default());
        let r = |t: &str| jinja.render_str(t, ()).unwrap();
        assert_eq!(r("{{ none | default('x', true) }}"), "x");
        assert_eq!(r("{{ missing | default('x') }}"), "x");
        assert_eq!(r("{{ none | bool }}"), "False");
        assert_eq!(r("{{ none | tojson }}"), "null");
        assert_eq!(r("{{ missing | upper | default('x') }}"), "x");
    }

    #[test]
    fn a_present_subject_reaches_the_builtin() {
        let jinja = build_pure_engine_env(Default::default());
        let r = |t: &str| jinja.render_str(t, ()).unwrap();
        assert_eq!(r("{{ 'ab' | upper }}"), "AB");
        assert_eq!(r("{{ [1, 2] | sum }}"), "3");
        assert_eq!(r("{{ ['a', 'b'] | join('-') }}"), "a-b");
        assert_eq!(r("{{ 3.4 | round | int }}"), "3");
    }

    /// The guarded builtins answer a present subject exactly as minijinja's
    /// own do, the ones it registers as private variants included:
    /// `replace(count=)`, `length`, `items`, `dictsort`, `reverse`.
    #[test]
    fn a_guarded_builtin_is_the_registered_one() {
        let ours = build_pure_engine_env(Default::default());
        let theirs = Environment::new();
        for t in [
            "{{ 'aaa' | replace('a', 'b', count=1) }}",
            "{{ 'aaa' | replace('a', 'b', 2) }}",
            "{{ [1, 2] | length }}",
            "{{ 'abc' | count }}",
            "{{ {'b': 1, 'a': 2} | items | list }}",
            "{{ {'b': 1, 'A': 2} | dictsort }}",
            "{{ {'b': 1, 'a': 2} | dictsort(by='value', reverse=true) }}",
            "{{ [1, 2, 3] | reverse | list }}",
            "{{ 'abc' | reverse }}",
            "{{ [3, 1] | first }}{{ [3, 1] | last }}",
            "{{ 'a\nb' | indent(2, true) }}",
            "{{ [{'k': 1}, {'k': 1}] | groupby('k') | list }}",
        ] {
            assert_eq!(
                ours.render_str(t, ()).unwrap(),
                theirs.render_str(t, ()).unwrap(),
                "{t}"
            );
        }
        let r = |t: &str| ours.render_str(t, ()).unwrap();
        assert_eq!(r("{{ 'aaa' | replace('a', 'b', count=1) }}"), "baa");
        assert_eq!(
            r("{{ {'b': 1, 'a': 2} | items | list }}"),
            "[('a', 2), ('b', 1)]"
        );
        for f in [
            "replace('a', 'b', count=1)",
            "length",
            "count",
            "items",
            "dictsort",
            "reverse",
        ] {
            for absent in ["none", "missing"] {
                assert_eq!(
                    r(&format!("{{{{ ({absent} | {f}) is undefined }}}}")),
                    "True",
                    "{absent} | {f}"
                );
            }
        }
    }

    #[test]
    fn parsers_and_paths_read_empty_as_absent() {
        let jinja = build_pure_engine_env(Default::default());
        let mut state = jinja.empty_state();
        for name in [
            "fromjson",
            "fromini",
            "basename",
            "dirname",
            "fromtoml",
            "fromyaml",
            "expanduser",
            "ext",
        ] {
            let got = state.apply_filter(name, &[MjValue::from("")]).unwrap();
            assert!(got.is_undefined(), "{name} gave {got:?} for ''");
        }
    }
}
