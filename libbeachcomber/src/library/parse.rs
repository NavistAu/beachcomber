//! Text to data, and text patterns: `fromjson`, `fromini`, `fromtoml`,
//! `fromyaml`, `regex_search` and `regex_replace`.

use super::{UNDEFINED, arguments, is_absent, is_absent_text, text, whole};
use crate::engine::{EngineCtx, op_error};
use minijinja::value::{Kwargs, Rest, Value as MjValue};
use minijinja::{Environment, Error, ErrorKind};
use std::sync::Arc;

pub(super) fn add_fromjson(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("fromjson", fromjson_filter);
}

pub(super) fn add_fromtoml(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("fromtoml", fromtoml);
}

pub(super) fn add_fromyaml(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("fromyaml", fromyaml);
}

pub(super) fn add_regex_search(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("regex_search", regex_search);
}

pub(super) fn add_regex_replace(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter(
        "regex_replace",
        |value: MjValue, args: Rest<MjValue>, kwargs: Kwargs| -> Result<MjValue, Error> {
            let [pattern, repl, count] = arguments(
                "regex_replace",
                &args,
                &kwargs,
                ["pattern", "repl", "count"],
            )?;
            regex_replace(&value, pattern, repl, count)
        },
    );
}

pub(super) fn add_fromini(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("fromini", fromini);
}

/// `{{ text | fromjson }}` — parse `text` as JSON into a value the template
/// can navigate: `(exec(['tool', 'info', '-o', 'json']) | fromjson).context`.
///
/// Text that is not JSON is an `InvalidOperation` error carrying serde's
/// message, so a tool that printed something else fails at the filter rather
/// than rendering `undefined` three attributes later.
fn fromjson_filter(value: MjValue) -> Result<MjValue, Error> {
    if is_absent_text(&value) {
        return Ok(UNDEFINED);
    }
    serde_json::from_str::<serde_json::Value>(&text("fromjson", &value)?)
        .map(|v| MjValue::from_serialize(&v))
        .map_err(|e| Error::new(ErrorKind::InvalidOperation, format!("fromjson(): {e}")))
}

/// `s | fromini`: section -> {key: string}. Keys before any section land
/// under `""`.
fn fromini(value: MjValue) -> Result<MjValue, Error> {
    if is_absent_text(&value) {
        return Ok(UNDEFINED);
    }
    let source = text("fromini", &value)?;
    let ini = ini::Ini::load_from_str(&source)
        .map_err(|e| Error::new(ErrorKind::InvalidOperation, format!("fromini(): {e}")))?;
    let mut out = serde_json::Map::new();
    for (section, props) in ini.iter() {
        if section.is_none() && props.is_empty() {
            continue;
        }
        let table = out
            .entry(section.unwrap_or("").to_string())
            .or_insert_with(|| serde_json::json!({}));
        for (k, v) in props.iter() {
            table[k] = serde_json::Value::String(v.to_string());
        }
    }
    Ok(MjValue::from_serialize(serde_json::Value::Object(out)))
}

/// `s | fromtoml`: a TOML document as a map. A datetime is its text.
fn fromtoml(value: MjValue) -> Result<MjValue, Error> {
    if is_absent_text(&value) {
        return Ok(UNDEFINED);
    }
    let doc: toml::Table = text("fromtoml", &value)?
        .parse()
        .map_err(|e: toml::de::Error| op_error(format!("fromtoml(): {}", e.message())))?;
    Ok(MjValue::from_serialize(toml_json(toml::Value::Table(doc))))
}

fn toml_json(v: toml::Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        toml::Value::String(s) => J::String(s),
        toml::Value::Integer(i) => J::from(i),
        toml::Value::Float(f) => {
            serde_json::Number::from_f64(f).map_or_else(|| J::String(f.to_string()), J::Number)
        }
        toml::Value::Boolean(b) => J::Bool(b),
        toml::Value::Datetime(d) => J::String(d.to_string()),
        toml::Value::Array(a) => J::Array(a.into_iter().map(toml_json).collect()),
        toml::Value::Table(t) => J::Object(t.into_iter().map(|(k, v)| (k, toml_json(v))).collect()),
    }
}

/// `s | fromyaml`: one YAML document as data. More than one document is an
/// error; text holding none (only comments) is undefined.
fn fromyaml(value: MjValue) -> Result<MjValue, Error> {
    if is_absent_text(&value) {
        return Ok(UNDEFINED);
    }
    let mut docs = yaml_rust2::YamlLoader::load_from_str(&text("fromyaml", &value)?)
        .map_err(|e| op_error(format!("fromyaml(): {e}")))?;
    match docs.len() {
        0 => Ok(UNDEFINED),
        1 => Ok(MjValue::from_serialize(yaml_json(docs.remove(0))?)),
        n => Err(op_error(format!("fromyaml(): {n} documents; expected one"))),
    }
}

fn yaml_json(v: yaml_rust2::Yaml) -> Result<serde_json::Value, Error> {
    use serde_json::Value as J;
    use yaml_rust2::Yaml;
    Ok(match v {
        Yaml::Integer(i) => J::from(i),
        Yaml::Real(r) => r
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or(J::String(r), J::Number),
        Yaml::String(s) => J::String(s),
        Yaml::Boolean(b) => J::Bool(b),
        Yaml::Array(a) => J::Array(a.into_iter().map(yaml_json).collect::<Result<_, _>>()?),
        Yaml::Hash(h) => J::Object(
            h.into_iter()
                .map(|(k, v)| Ok((yaml_key(k)?, yaml_json(v)?)))
                .collect::<Result<_, Error>>()?,
        ),
        Yaml::Null | Yaml::BadValue | Yaml::Alias(_) => J::Null,
    })
}

/// A map key: a scalar, written as text.
fn yaml_key(k: yaml_rust2::Yaml) -> Result<String, Error> {
    use yaml_rust2::Yaml;
    match k {
        Yaml::String(s) | Yaml::Real(s) => Ok(s),
        Yaml::Integer(i) => Ok(i.to_string()),
        Yaml::Boolean(b) => Ok(b.to_string()),
        Yaml::Null => Ok("null".to_string()),
        _ => Err(op_error("fromyaml(): a map key must be a scalar")),
    }
}

/// A pattern argument, compiled: text in Rust `regex` syntax.
fn pattern(name: &str, v: Option<MjValue>) -> Result<regex::Regex, Error> {
    let Some(v) = v.filter(|v| !is_absent(v)) else {
        return Err(op_error(format!("{name}(): takes a pattern")));
    };
    let Some(p) = v.as_str() else {
        return Err(op_error(format!("{name}(): the pattern must be text")));
    };
    regex::Regex::new(p).map_err(|e| {
        // The message is several lines, the last of which says what is wrong.
        let why = e.to_string();
        let why = why.lines().last().unwrap_or("").trim();
        op_error(format!("{name}(): invalid pattern {p:?}: {why}"))
    })
}

/// `s | regex_search(pattern)`: the first match as a list, the whole match and
/// then each group (`none` for one that did not take part). No match is
/// undefined. The pattern is checked first.
fn regex_search(value: MjValue, pattern_arg: MjValue) -> Result<MjValue, Error> {
    let re = pattern("regex_search", Some(pattern_arg))?;
    if is_absent(&value) {
        return Ok(UNDEFINED);
    }
    let s = text("regex_search", &value)?;
    Ok(re.captures(&s).map_or(UNDEFINED, |caps| {
        MjValue::from_serialize(
            caps.iter()
                .map(|g| g.map(|m| m.as_str()))
                .collect::<Vec<_>>(),
        )
    }))
}

/// `s | regex_replace(pattern, repl, count=0)`: matches replaced by `repl`
/// (`$1` and `${name}` are groups), all of them when `count` is 0, else the
/// first `count`. The arguments are checked first.
fn regex_replace(
    value: &MjValue,
    pattern_arg: Option<MjValue>,
    repl: Option<MjValue>,
    count: Option<MjValue>,
) -> Result<MjValue, Error> {
    let re = pattern("regex_replace", pattern_arg)?;
    let repl = match repl.filter(|r| !is_absent(r)) {
        Some(r) => text("regex_replace", &r)?,
        None => return Err(op_error("regex_replace(): takes a replacement")),
    };
    let limit = match count.filter(|c| !is_absent(c)) {
        None => 0,
        Some(c) => usize::try_from(whole("regex_replace", "count", &c)?)
            .map_err(|_| op_error("regex_replace(): count must not be negative"))?,
    };
    if is_absent(value) {
        return Ok(UNDEFINED);
    }
    let s = text("regex_replace", value)?;
    Ok(MjValue::from(
        re.replacen(&s, limit, repl.as_str()).into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::build_engine_env;
    use crate::library::testing::eval_json;

    #[test]
    fn fromjson_parses_an_object_that_round_trips() {
        let v = fromjson_filter(MjValue::from(
            r#"{"context": "mycluster", "nodes": [1, 2]}"#,
        ))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&v).unwrap(),
            serde_json::json!({"context": "mycluster", "nodes": [1, 2]})
        );
    }

    #[test]
    fn fromjson_result_is_navigable_in_an_expression() {
        // The shipped `talos.toml` shape: parse a tool's JSON, then read one key.
        let env = build_engine_env(Default::default());
        let expr = env.compile_expression("(s | fromjson).context").unwrap();
        let out = expr
            .eval(minijinja::context! { s => r#"{"context": "mycluster", "roles": null}"# })
            .unwrap();
        assert_eq!(out.as_str(), Some("mycluster"));
    }

    #[test]
    fn fromjson_rejects_text_that_is_not_json() {
        let err = fromjson_filter(MjValue::from("error: current-context is not set")).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidOperation);
        assert!(
            err.to_string().contains("fromjson():"),
            "error should name the filter, got: {err}"
        );
    }

    #[test]
    fn fromini_sections() {
        let ini = "top=1\n[core]\nname = x\nflag=yes\n[profile dev]\nregion=eu\n";
        let out = eval_json(&format!("'{}' | fromini", ini.replace('\n', "\\n")));
        assert_eq!(
            out,
            r#"{"":{"top":"1"},"core":{"flag":"yes","name":"x"},"profile dev":{"region":"eu"}}"#
        );
        assert_eq!(eval_json("'[s]\\nk=v' | fromini"), r#"{"s":{"k":"v"}}"#);
    }

    fn ok(expr: &str) -> String {
        crate::library::testing::eval(expr).unwrap_or_else(|e| panic!("{expr}: {e}"))
    }

    fn err(expr: &str) -> String {
        crate::library::testing::eval(expr).unwrap_err()
    }

    #[test]
    fn fromtoml_parses_a_document() {
        let doc = "[package]\\nname = \"x\"\\nversion = \"0.9.1\"\\n[deps]\\nn = 3\\nf = 1.5\\nok = true\\nl = [1, 2]\\nwhen = 2026-10-07T03:00:00Z\\n";
        assert_eq!(
            eval_json(&format!("'{doc}' | fromtoml")),
            r#"{"deps":{"f":1.5,"l":[1,2],"n":3,"ok":true,"when":"2026-10-07T03:00:00Z"},"package":{"name":"x","version":"0.9.1"}}"#
        );
        assert_eq!(
            ok(&format!("'{doc}' | fromtoml | pluck('package.version')")),
            "0.9.1"
        );
    }

    #[test]
    fn fromtoml_rejects_text_that_is_not_toml() {
        let e = err("'a = ' | fromtoml");
        assert!(e.contains("fromtoml(): "), "{e}");
        let e = err("[1] | fromtoml");
        assert!(e.contains("fromtoml(): expected text"), "{e}");
        for absent in ["''", "none", "nope"] {
            assert_eq!(ok(&format!("({absent} | fromtoml) is undefined")), "true");
        }
    }

    #[test]
    fn fromyaml_parses_one_document() {
        let kube = "current-context: dev\\nclusters:\\n  - name: dev\\n    port: 6443\\n    tls: true\\n    ratio: 0.5\\n    note: ~\\n";
        assert_eq!(
            eval_json(&format!("'{kube}' | fromyaml")),
            r#"{"clusters":[{"name":"dev","note":null,"port":6443,"ratio":0.5,"tls":true}],"current-context":"dev"}"#
        );
        assert_eq!(
            ok(&format!("'{kube}' | fromyaml | pluck('current-context')")),
            "dev"
        );
        assert_eq!(
            eval_json("'a: &x [1, 2]\\nb: *x\\n1: one\\n' | fromyaml"),
            r#"{"1":"one","a":[1,2],"b":[1,2]}"#
        );
        assert_eq!(eval_json("'- 1\\n- two\\n' | fromyaml"), r#"[1,"two"]"#);
    }

    #[test]
    fn fromyaml_rejects_bad_text_and_several_documents() {
        let e = err("'a: [1' | fromyaml");
        assert!(e.contains("fromyaml(): "), "{e}");
        let e = err("'a: 1\\n---\\nb: 2\\n' | fromyaml");
        assert!(e.contains("fromyaml(): 2 documents; expected one"), "{e}");
        let e = err("{} | fromyaml");
        assert!(e.contains("fromyaml(): expected text"), "{e}");
        for absent in ["''", "none", "nope", "'# only a comment\\n'"] {
            assert_eq!(
                ok(&format!("({absent} | fromyaml) is undefined")),
                "true",
                "{absent}"
            );
        }
    }

    #[test]
    fn regex_search_gives_the_match_and_its_groups() {
        assert_eq!(
            eval_json(r"'v22.1.0' | regex_search('(\d+)\.(\d+)')"),
            r#"["22.1","22","1"]"#
        );
        assert_eq!(ok(r"('v22.1.0' | regex_search('(\d+)\.(\d+)'))[1]"), "22");
        assert_eq!(eval_json("'abc' | regex_search('b')"), r#"["b"]"#);
        // A group that took no part is none, in its place.
        assert_eq!(
            eval_json("'b' | regex_search('(a)?(b)')"),
            r#"[ "b",null,"b"]"#.replace(' ', "")
        );
        assert_eq!(
            eval_json("'/worktrees/feat-x/' | regex_search('/worktrees/([^/\\s]+)')"),
            r#"["/worktrees/feat-x","feat-x"]"#
        );
        // First match only.
        assert_eq!(
            eval_json("'a1 b2' | regex_search('[a-z](\\d)')"),
            r#"["a1","1"]"#
        );
        assert_eq!(eval_json("12345 | regex_search('3.')"), r#"["34"]"#);
    }

    #[test]
    fn regex_search_without_a_match_or_subject_is_undefined() {
        assert_eq!(ok("('abc' | regex_search('z')) is undefined"), "true");
        assert_eq!(ok("(none | regex_search('a')) is undefined"), "true");
        assert_eq!(ok("(nope | regex_search('a')) is undefined"), "true");
        assert_eq!(ok("('' | regex_search('a')) is undefined"), "true");
        assert_eq!(ok("('' | regex_search('^')) is defined"), "true");
    }

    #[test]
    fn regex_search_checks_the_pattern_first() {
        for subject in ["'abc'", "none"] {
            let e = err(&format!("{subject} | regex_search('(')"));
            assert!(e.contains("regex_search(): invalid pattern \"(\""), "{e}");
        }
        let e = err("'a' | regex_search(1)");
        assert!(
            e.contains("regex_search(): the pattern must be text"),
            "{e}"
        );
        let e = err("[1] | regex_search('a')");
        assert!(e.contains("regex_search(): expected text"), "{e}");
    }

    #[test]
    fn regex_replace_replaces_all_or_the_first_n() {
        assert_eq!(ok("'feat/x' | regex_replace('^feat/', '')"), "x");
        assert_eq!(ok("'a-b-c' | regex_replace('-', '+')"), "a+b+c");
        assert_eq!(ok("'a-b-c' | regex_replace('-', '+', 1)"), "a+b-c");
        assert_eq!(ok("'a-b-c' | regex_replace('-', '+', count=2)"), "a+b+c");
        assert_eq!(ok("'a-b-c' | regex_replace('-', '+', count=0)"), "a+b+c");
        assert_eq!(
            ok("'v22.1' | regex_replace('(\\d+)\\.(\\d+)', '$2.$1')"),
            "v1.22"
        );
        assert_eq!(
            ok("'v22.1' | regex_replace('(?P<maj>\\d+)\\.', '${maj}x.')"),
            "v22x.1"
        );
        assert_eq!(ok("'abc' | regex_replace('z', 'y')"), "abc");
        assert_eq!(ok("'abc' | regex_replace('b', '')"), "ac");
        assert_eq!(ok("123 | regex_replace('2', 'x')"), "1x3");
    }

    #[test]
    fn regex_replace_checks_its_arguments_first() {
        assert_eq!(ok("(none | regex_replace('a', 'b')) is undefined"), "true");
        assert_eq!(ok("(nope | regex_replace('a', 'b')) is undefined"), "true");
        for subject in ["'abc'", "none"] {
            let e = err(&format!("{subject} | regex_replace('(', 'x')"));
            assert!(e.contains("regex_replace(): invalid pattern"), "{e}");
            let e = err(&format!("{subject} | regex_replace('a', 'x', -1)"));
            assert!(
                e.contains("regex_replace(): count must not be negative"),
                "{e}"
            );
            let e = err(&format!("{subject} | regex_replace('a', 'x', 'many')"));
            assert!(
                e.contains("regex_replace(): count must be a whole number"),
                "{e}"
            );
            let e = err(&format!("{subject} | regex_replace('a')"));
            assert!(e.contains("regex_replace(): takes a replacement"), "{e}");
            let e = err(&format!("{subject} | regex_replace"));
            assert!(e.contains("regex_replace(): takes a pattern"), "{e}");
        }
        let e = err("'a' | regex_replace('a', 'b', 1, 2)");
        assert!(
            e.contains("regex_replace(): takes at most 3 arguments"),
            "{e}"
        );
    }
}
