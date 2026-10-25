//! Objects and lists: `pluck`, `merge` and `map`.

use super::{is_blank, none};
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

/// `v | pluck(path)`: deep get, `none` on any miss. `path` is an address
/// in the key grammar ([`crate::key`]), the one `comb get` reads: names
/// joined by `.`, `[n]` for an index (negative from the end), `['k.y']` for a
/// name holding a dot. A path the grammar cannot read is an error.
fn pluck(value: MjValue, path: String) -> Result<MjValue, Error> {
    let segments = crate::key::parse_address(&path)
        .ok_or_else(|| op_error(format!("pluck(): malformed path '{path}'")))?;
    let mut cur = value;
    for segment in segments {
        cur = match cur.kind() {
            ValueKind::Map => cur.get_attr(&segment)?,
            ValueKind::Seq => {
                let len = cur.len().unwrap_or(0);
                match crate::key::array_index(&segment, len).filter(|i| *i < len) {
                    Some(i) => cur.get_item_by_index(i)?,
                    None => return Ok(none()),
                }
            }
            _ => return Ok(none()),
        };
        if is_blank(&cur) {
            return Ok(none());
        }
    }
    Ok(cur)
}

/// Deep merge, later wins; a non-object replaces; a `null` leaf removes. A
/// `none` or undefined operand contributes nothing: it does not replace what
/// the others gave.
pub fn merge_values(values: impl Iterator<Item = MjValue>) -> Result<MjValue, Error> {
    let mut acc = serde_json::Value::Null;
    for v in values.filter(|v| !is_blank(v)) {
        let json = serde_json::to_value(&v).map_err(|e| op_error(format!("merge(): {e}")))?;
        acc = merge_json(acc, json);
    }
    Ok(MjValue::from_serialize(acc))
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
    if value.kind() != ValueKind::Map {
        return minijinja::filters::map(state, value, args).map(MjValue::from);
    }
    let (pos, kwargs): (&[MjValue], Kwargs) = minijinja::value::from_args(&args)?;
    let name = match pos.first().and_then(|a| a.as_str()) {
        Some(name) if !kwargs.has("attribute") => name.to_string(),
        _ => return Err(op_error("map over an object takes a filter name")),
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
        assert_eq!(p("a.missing.c"), "none");
        assert_eq!(p("node[5]"), "none");
        assert_eq!(p("a[0]"), "none");
        assert_eq!(p("node.version"), "none");
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
            err.contains("map over an object takes a filter name"),
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
