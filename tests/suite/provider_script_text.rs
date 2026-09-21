// TDD: validate script provider "text" output mode wraps stdout as { value: <stdout> }.

use beachcomber::config::ScriptProviderConfig;
use beachcomber::provider::script::ScriptProvider;
use beachcomber::provider::{Provider, Value};

fn base_cfg(cmd: &str, output: &str) -> ScriptProviderConfig {
    ScriptProviderConfig {
        command: cmd.to_string(),
        output: Some(output.to_string()),
        ..Default::default()
    }
}

#[test]
fn text_output_wraps_stdout_in_value_field() {
    let provider = ScriptProvider::new("myscript", base_cfg("echo hello world", "text"));
    let sources = provider.sources();
    let result = sources[0].execute(None);
    assert!(!result.fields.is_empty(), "text output should not be empty");
    let value = result.fields.get("value").expect("value field missing");
    match value {
        Value::String(s) => assert_eq!(s, "hello world"),
        other => panic!("expected String, got {:?}", other),
    }
}

#[test]
fn text_output_trims_trailing_whitespace() {
    let provider = ScriptProvider::new("trim", base_cfg("printf 'trim-me\\n\\n'", "text"));
    let sources = provider.sources();
    let result = sources[0].execute(None);
    assert!(!result.fields.is_empty(), "text output should not be empty");
    let v = result.fields.get("value").expect("value field missing");
    match v {
        Value::String(s) => assert_eq!(s, "trim-me"),
        other => panic!("expected String, got {:?}", other),
    }
}

#[test]
fn text_output_empty_stdout_returns_empty() {
    let provider = ScriptProvider::new("empty", base_cfg("true", "text"));
    let sources = provider.sources();
    let result = sources[0].execute(None);
    assert!(result.fields.is_empty());
}

#[test]
fn text_output_preserves_multiline_content() {
    let provider = ScriptProvider::new("multi", base_cfg("printf 'line1\\nline2'", "text"));
    let sources = provider.sources();
    let result = sources[0].execute(None);
    match result.fields.get("value").unwrap() {
        Value::String(s) => assert_eq!(s, "line1\nline2"),
        other => panic!("expected String, got {:?}", other),
    }
}

#[test]
fn unknown_output_format_falls_through_to_json_parser() {
    let provider = ScriptProvider::new("typo", base_cfg("echo not-json-data", "typo"));
    let sources = provider.sources();
    let result = sources[0].execute(None);
    assert!(result.fields.is_empty());
}
