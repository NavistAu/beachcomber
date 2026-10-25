//! Text to data: `fromjson` and `fromini`.

use crate::engine::{EngineCtx, op_error};
use minijinja::{Environment, Error, ErrorKind};
use std::sync::Arc;

pub(super) fn add_fromjson(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("fromjson", fromjson_filter);
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
fn fromjson_filter(value: String) -> Result<minijinja::Value, Error> {
    serde_json::from_str::<serde_json::Value>(&value)
        .map(|v| minijinja::Value::from_serialize(&v))
        .map_err(|e| Error::new(ErrorKind::InvalidOperation, format!("fromjson: {e}")))
}

/// `s | fromini`: section -> {key: string}. Keys before any section land
/// under `""`.
fn fromini(text: String) -> Result<minijinja::Value, Error> {
    let ini = ini::Ini::load_from_str(&text).map_err(|e| op_error(format!("fromini: {e}")))?;
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
    Ok(minijinja::Value::from_serialize(serde_json::Value::Object(
        out,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::build_engine_env;
    use crate::library::testing::eval_json;

    #[test]
    fn fromjson_parses_an_object_that_round_trips() {
        let v =
            fromjson_filter(r#"{"context": "mycluster", "nodes": [1, 2]}"#.to_string()).unwrap();
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
        let err = fromjson_filter("error: current-context is not set".to_string()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidOperation);
        assert!(
            err.to_string().contains("fromjson"),
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
}
