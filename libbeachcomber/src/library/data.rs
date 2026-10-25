//! Objects, lists and numbers: `pluck`, `merge`, `map`, `pick`, `omit` and
//! `clamp`.

use super::{UNDEFINED, is_absent};
use crate::engine::{EngineCtx, op_error};
use minijinja::value::{Kwargs, Rest, Value as MjValue, ValueKind};
use minijinja::{Environment, Error, State};
use std::sync::Arc;

pub(super) fn add_pluck(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("pluck", pluck);
}

pub(super) fn add_merge(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("merge", |first: MjValue, rest: Rest<MjValue>| {
        merge_values(std::iter::once(first).chain(rest.iter().cloned()))
    });
    env.add_function("merge", |args: Rest<MjValue>| match args.as_slice() {
        [list] if list.kind() == ValueKind::Seq => merge_values(list.try_iter()?),
        _ => merge_values(args.iter().cloned()),
    });
}

pub(super) fn add_map(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("map", map);
}

pub(super) fn add_pick(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("pick", |obj: MjValue, keys: Rest<MjValue>| {
        select_keys("pick", &obj, &keys, true)
    });
}

pub(super) fn add_omit(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("omit", |obj: MjValue, keys: Rest<MjValue>| {
        select_keys("omit", &obj, &keys, false)
    });
}

pub(super) fn add_clamp(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("clamp", clamp);
}

/// `pick` (`keep`) and `omit` (not `keep`): the object with only, or without,
/// the named keys. The keys are checked first, then the subject.
fn select_keys(name: &str, obj: &MjValue, keys: &[MjValue], keep: bool) -> Result<MjValue, Error> {
    let mut wanted = Vec::with_capacity(keys.len());
    for key in keys {
        match key.as_str() {
            Some(k) => wanted.push(k),
            None => return Err(op_error(format!("{name}(): a key must be text, not {key}"))),
        }
    }
    if is_absent(obj) {
        return Ok(UNDEFINED);
    }
    if obj.kind() != ValueKind::Map {
        return Err(op_error(format!("{name}(): expected an object, not {obj}")));
    }
    let mut out = Vec::new();
    for key in obj.try_iter()? {
        let listed = key.as_str().is_some_and(|k| wanted.contains(&k));
        if listed == keep {
            let value = obj.get_item(&key)?;
            out.push((key, value));
        }
    }
    Ok(MjValue::from_iter(out))
}

/// One side of `clamp`: a number, or `none` (or undefined) for an open side.
fn bound(side: &str, v: &MjValue) -> Result<Option<f64>, Error> {
    if is_absent(v) {
        return Ok(None);
    }
    number("clamp", side, v).map(Some)
}

fn number(name: &str, what: &str, v: &MjValue) -> Result<f64, Error> {
    match v.kind() {
        ValueKind::Number => f64::try_from(v.clone())
            .map_err(|_| op_error(format!("{name}(): {what} {v} is not a number"))),
        _ => Err(op_error(format!("{name}(): {what} {v} is not a number"))),
    }
}

/// `n | clamp(lo, hi)`: `n` held to `lo..=hi`. A `none` bound is open. The
/// bounds are checked first.
fn clamp(n: MjValue, lo: MjValue, hi: MjValue) -> Result<MjValue, Error> {
    let (low, high) = (bound("lo", &lo)?, bound("hi", &hi)?);
    if let (Some(l), Some(h)) = (low, high)
        && l > h
    {
        return Err(op_error(format!("clamp(): lo {lo} is above hi {hi}")));
    }
    if is_absent(&n) {
        return Ok(UNDEFINED);
    }
    let x = number("clamp", "the subject", &n)?;
    Ok(match (low, high) {
        (Some(l), _) if x < l => lo,
        (_, Some(h)) if x > h => hi,
        _ => n,
    })
}

/// `v | pluck(path)`: deep get, `none` on any miss. `path` is an address
/// in the key grammar ([`crate::key`]), the one `comb get` reads: names
/// joined by `.`, `[n]` for an index (negative from the end), `['k.y']` for a
/// name holding a dot. A path the grammar cannot read is an error.
fn pluck(value: MjValue, path: String) -> Result<MjValue, Error> {
    let segments = crate::key::parse_address(&path)
        .ok_or_else(|| op_error(format!("pluck(): malformed path '{path}'")))?;
    if is_absent(&value) {
        return Ok(UNDEFINED);
    }
    let mut cur = value;
    for segment in segments {
        cur = match cur.kind() {
            ValueKind::Map => cur.get_attr(&segment)?,
            ValueKind::Seq => {
                let len = cur.len().unwrap_or(0);
                match crate::key::array_index(&segment, len).filter(|i| *i < len) {
                    Some(i) => cur.get_item_by_index(i)?,
                    None => return Ok(UNDEFINED),
                }
            }
            _ => return Ok(UNDEFINED),
        };
        if is_absent(&cur) {
            return Ok(UNDEFINED);
        }
    }
    Ok(cur)
}

/// Deep merge, later wins; a non-object replaces; a `null` leaf removes. A
/// `none` or undefined operand contributes nothing: it does not replace what
/// the others gave, and with nothing left the result is undefined.
pub fn merge_values(values: impl Iterator<Item = MjValue>) -> Result<MjValue, Error> {
    let mut acc = None;
    for v in values.filter(|v| !is_absent(v)) {
        let json = serde_json::to_value(&v).map_err(|e| op_error(format!("merge(): {e}")))?;
        acc = Some(merge_json(acc.unwrap_or(serde_json::Value::Null), json));
    }
    Ok(acc.map_or(UNDEFINED, MjValue::from_serialize))
}

fn merge_json(base: serde_json::Value, over: serde_json::Value) -> serde_json::Value {
    use serde_json::Value::Object;
    match (base, over) {
        (Object(mut a), Object(b)) => {
            for (k, v) in b {
                if v.is_null() {
                    a.remove(&k);
                } else {
                    let merged = match a.remove(&k) {
                        Some(old) => merge_json(old, v),
                        None => strip_nulls(v),
                    };
                    a.insert(k, merged);
                }
            }
            Object(a)
        }
        (_, over) => strip_nulls(over),
    }
}

fn strip_nulls(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(m) => serde_json::Value::Object(
            m.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, strip_nulls(v)))
                .collect(),
        ),
        v => v,
    }
}

/// `map`, extended to objects: `obj | map('filter', args…)` applies the
/// filter to each value and keeps the keys. Anything else is jinja's `map`.
fn map(state: &State, value: MjValue, args: Rest<MjValue>) -> Result<MjValue, Error> {
    if is_absent(&value) {
        return Ok(UNDEFINED);
    }
    if value.kind() != ValueKind::Map {
        return minijinja::filters::map(state, value, args).map(MjValue::from);
    }
    let (pos, kwargs): (&[MjValue], Kwargs) = minijinja::value::from_args(&args)?;
    let name = match pos.first().and_then(|a| a.as_str()) {
        Some(name) if !kwargs.has("attribute") => name.to_string(),
        _ => return Err(op_error("map(): over an object takes a filter name")),
    };
    kwargs.assert_all_used()?;
    let mut out = Vec::new();
    for key in value.try_iter()? {
        let item = value.get_item(&key)?;
        let mut call = vec![item];
        call.extend(pos[1..].iter().cloned());
        out.push((key, state.apply_filter(&name, &call)?));
    }
    Ok(MjValue::from_iter(out))
}

#[cfg(test)]
mod tests {
    use crate::library::testing::{eval, eval_json};

    #[test]
    fn pluck_path_forms() {
        let data = r#"'{"node":[{"version":"22.1.0"},{"version":"20.0.0"}],"k.y":7,"a":{"b":{"c":1}}}' | fromjson"#;
        let p = |path: &str| eval(&format!("({data}) | pluck('{path}')")).unwrap();
        assert_eq!(p("node[0].version"), "22.1.0");
        assert_eq!(p("node[-1].version"), "20.0.0");
        assert_eq!(p("a.b.c"), "1");
        assert_eq!(p(r#"[\"k.y\"]"#), "7");
        for miss in ["a.missing.c", "node[5]", "a[0]", "node.version"] {
            assert_eq!(
                eval(&format!("(({data}) | pluck('{miss}')) is undefined")).unwrap(),
                "true",
                "{miss}"
            );
        }
        assert_eq!(eval("(none | pluck('a')) is undefined").unwrap(), "true");
        assert_eq!(
            p("node.-1.version"),
            "20.0.0",
            "an index is .n as well as [n]"
        );
        for malformed in ["a[", "node[01]", "node[-0]", "a..b"] {
            let err = eval(&format!("({data}) | pluck('{malformed}')")).unwrap_err();
            assert!(err.contains("pluck()"), "{malformed}: {err}");
        }
    }

    #[test]
    fn map_over_an_object_keeps_keys() {
        let mise = r#"'{"node":[{"version":"22.1.0"}]}' | fromjson"#;
        assert_eq!(
            eval_json(&format!("({mise}) | map('pluck', '[0].version')")),
            r#"{"node":"22.1.0"}"#
        );
    }

    #[test]
    fn map_over_an_object_with_attribute_is_an_error() {
        let err = eval(r#"('{"a":1}' | fromjson) | map(attribute='x')"#).unwrap_err();
        assert!(
            err.contains("map(): over an object takes a filter name"),
            "{err}"
        );
    }

    #[test]
    fn map_over_a_list_is_jinjas_map() {
        assert_eq!(
            eval("['a', 'b'] | map('upper') | join(',')").unwrap(),
            "A,B"
        );
        assert_eq!(
            eval(r#"[{"v":1},{"v":2}] | map(attribute='v') | join(',')"#).unwrap(),
            "1,2"
        );
    }

    #[test]
    fn pick_and_omit_keep_or_drop_named_keys() {
        let aws = r#"{"profile":"dev","region":"eu","token":"x"}"#;
        assert_eq!(
            eval_json(&format!("({aws}) | pick('profile', 'region')")),
            r#"{"profile":"dev","region":"eu"}"#
        );
        assert_eq!(
            eval_json(&format!("({aws}) | omit('token')")),
            r#"{"profile":"dev","region":"eu"}"#
        );
        // A key the object lacks is not an error.
        assert_eq!(eval_json(&format!("({aws}) | pick('nope')")), "{}");
        assert_eq!(
            eval_json(&format!(
                "({aws}) | omit('nope', 'token', 'region', 'profile')"
            )),
            "{}"
        );
        // Values keep their types.
        assert_eq!(
            eval_json("{'a': [1], 'b': none, 'c': {'d': 2}} | pick('a', 'c')"),
            r#"{"a":[1],"c":{"d":2}}"#
        );
    }

    #[test]
    fn pick_and_omit_of_nothing_are_undefined() {
        for f in ["pick('a')", "omit('a')"] {
            assert_eq!(eval(&format!("(none | {f}) is undefined")).unwrap(), "true");
            assert_eq!(eval(&format!("(nope | {f}) is undefined")).unwrap(), "true");
        }
    }

    #[test]
    fn pick_and_omit_reject_a_bad_subject_or_key() {
        for f in ["pick", "omit"] {
            let err = eval(&format!("[1] | {f}('a')")).unwrap_err();
            assert!(err.contains(&format!("{f}(): expected an object")), "{err}");
            let err = eval(&format!("'x' | {f}('a')")).unwrap_err();
            assert!(err.contains(&format!("{f}(): expected an object")), "{err}");
            let err = eval(&format!("{{'a': 1}} | {f}(3)")).unwrap_err();
            assert!(
                err.contains(&format!("{f}(): a key must be text, not 3")),
                "{err}"
            );
            // Arguments first: an absent subject with a bad key is the key error.
            let err = eval(&format!("none | {f}(3)")).unwrap_err();
            assert!(err.contains(&format!("{f}(): a key must be text")), "{err}");
        }
    }

    #[test]
    fn clamp_holds_a_number_to_its_bounds() {
        assert_eq!(eval("5 | clamp(0, 3)").unwrap(), "3");
        assert_eq!(eval("-5 | clamp(0, 3)").unwrap(), "0");
        assert_eq!(eval("2 | clamp(0, 3)").unwrap(), "2");
        assert_eq!(eval("1.5 | clamp(0, 3)").unwrap(), "1.5");
        assert_eq!(eval("0.7 | clamp(0, 1)").unwrap(), "0.7");
        assert_eq!(eval("1.5 | clamp(0, 1)").unwrap(), "1");
        // A `none` side is open.
        assert_eq!(eval("(4 - 6) | clamp(0, none)").unwrap(), "0");
        assert_eq!(eval("100 | clamp(0, none)").unwrap(), "100");
        assert_eq!(eval("100 | clamp(none, 10)").unwrap(), "10");
        assert_eq!(eval("-100 | clamp(none, none)").unwrap(), "-100");
        assert_eq!(eval("3 | clamp(3, 3)").unwrap(), "3");
    }

    #[test]
    fn clamp_of_nothing_is_undefined_after_its_bounds_are_checked() {
        assert_eq!(eval("(none | clamp(0, 1)) is undefined").unwrap(), "true");
        assert_eq!(
            eval("(nope | clamp(none, none)) is undefined").unwrap(),
            "true"
        );
        let err = eval("none | clamp(3, 1)").unwrap_err();
        assert!(err.contains("clamp(): lo 3 is above hi 1"), "{err}");
        let err = eval("none | clamp('a', 1)").unwrap_err();
        assert!(err.contains("clamp(): lo a is not a number"), "{err}");
    }

    #[test]
    fn clamp_rejects_a_bad_subject_or_bound() {
        for bad in [
            "'3' | clamp(0, 1)",
            "[1] | clamp(0, 1)",
            "true | clamp(0, 1)",
        ] {
            let err = eval(bad).unwrap_err();
            assert!(err.contains("clamp(): the subject"), "{bad}: {err}");
        }
        let err = eval("1 | clamp(0, 'x')").unwrap_err();
        assert!(err.contains("clamp(): hi x is not a number"), "{err}");
        let err = eval("1 | clamp(2, 1)").unwrap_err();
        assert!(err.contains("clamp(): lo 2 is above hi 1"), "{err}");
    }

    #[test]
    fn merge_later_wins_and_none_removes() {
        let a = r#"{"x":{"p":1,"q":2},"y":1,"z":[1]}"#;
        let b = r#"{"x":{"q":3,"r":4},"y":{"n":1},"z":[2]}"#;
        let j = eval_json;
        let want = r#"{"x":{"p":1,"q":3,"r":4},"y":{"n":1},"z":[2]}"#;
        assert_eq!(j(&format!("({a}) | merge({b})")), want);
        assert_eq!(j(&format!("merge({a}, {b})")), want);
        assert_eq!(j(&format!("merge([{a}, {b}])")), want);
        assert_eq!(
            j(&format!("({a}) | merge({{'x': {{'p': none}}, 'y': none}})")),
            r#"{"x":{"q":2},"z":[1]}"#
        );
    }
}
