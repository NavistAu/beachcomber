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
use minijinja::value::{Rest, Value as MjValue};
use minijinja::{Environment, Error, State, filters};

/// The builtins that take an absent subject as they are: they supply a default,
/// test truth, or show the value.
pub const EXEMPT: &[&str] = &[
    "default", "d", "bool", "tojson", "pprint", "safe", "escape", "e",
];

/// Call `inner`, the builtin, unless its subject is absent.
fn guarded(inner: &MjValue, state: &State, args: &[MjValue]) -> Result<MjValue, Error> {
    match args.first() {
        Some(subject) if is_absent(subject) => Ok(UNDEFINED),
        _ => inner.call(state, args),
    }
}

macro_rules! guard {
    ($jinja:expr, $($name:literal => $f:path),+ $(,)?) => {$(
        {
            let inner = MjValue::from_function($f);
            $jinja.add_filter($name, move |state: &State, args: Rest<MjValue>| {
                guarded(&inner, state, &args)
            });
        }
    )+};
}

/// Re-register minijinja's builtin filters (but those in [`EXEMPT`]) so an
/// absent subject gives undefined.
pub(crate) fn guard_builtins(jinja: &mut Environment<'_>) {
    guard!(jinja,
        "upper" => filters::upper,
        "lower" => filters::lower,
        "title" => filters::title,
        "capitalize" => filters::capitalize,
        "replace" => filters::replace,
        "length" => filters::length,
        "count" => filters::length,
        "dictsort" => filters::dictsort,
        "items" => filters::items,
        "reverse" => filters::reverse,
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
        "first" => filters::first,
        "last" => filters::last,
        "min" => filters::min,
        "max" => filters::max,
        "sort" => filters::sort,
        "list" => filters::list,
        "string" => filters::string,
        "slice" => filters::slice,
        "batch" => filters::batch,
        "indent" => filters::indent,
        "select" => filters::select,
        "selectattr" => filters::selectattr,
        "reject" => filters::reject,
        "rejectattr" => filters::rejectattr,
        "map" => filters::map,
        "groupby" => filters::groupby,
        "unique" => filters::unique,
        "chain" => filters::chain,
        "zip" => filters::zip,
        "format" => filters::format,
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
            "ramp" => vec![
                MjValue::from_serialize([50]),
                MjValue::from_serialize(["lo", "hi"]),
            ],
            "pick" | "omit" => vec![MjValue::from("a")],
            "clamp" => vec![MjValue::from(0), MjValue::from(1)],
            _ => vec![],
        }
    }

    #[test]
    fn every_filter_returns_undefined_for_an_absent_subject() {
        let jinja = build_pure_engine_env(Default::default());
        let state = jinja.empty_state();
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
        assert_eq!(r("{{ none | bool }}"), "false");
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

    #[test]
    fn parsers_and_paths_read_empty_as_absent() {
        let jinja = build_pure_engine_env(Default::default());
        let state = jinja.empty_state();
        for name in ["fromjson", "fromini", "basename", "dirname"] {
            let got = state.apply_filter(name, &[MjValue::from("")]).unwrap();
            assert!(got.is_undefined(), "{name} gave {got:?} for ''");
        }
    }
}
