// TDD: validate script provider "text" output mode wraps stdout as { value: <stdout> }.

use beachcomber::config::ScriptProviderConfig;
use beachcomber::provider::{Provider, Value};
use beachcomber::provider::script::ScriptProvider;

fn base_cfg(cmd: &str, output: &str) -> ScriptProviderConfig {
    let mut cfg = ScriptProviderConfig::default();
    cfg.command = cmd.to_string();
    cfg.output = Some(output.to_string());
    cfg
}

#[test]
fn text_output_wraps_stdout_in_value_field() {
    let provider = ScriptProvider::new("myscript", base_cfg("echo hello world", "text"));
    let result = provider.execute(None).expect("text output should not be None");
    let value = result.get("value").expect("value field missing");
    match value {
        Value::String(s) => assert_eq!(s, "hello world"),
        other => panic!("expected String, got {:?}", other),
    }
}

#[test]
fn text_output_trims_trailing_whitespace() {
    // echo adds a newline; the parser should trim trailing whitespace so value is clean.
    let provider = ScriptProvider::new("trim", base_cfg("printf 'trim-me\\n\\n'", "text"));
    let result = provider.execute(None).expect("text output should not be None");
    let v = result.get("value").expect("value field missing");
    match v {
        Value::String(s) => assert_eq!(s, "trim-me"),
        other => panic!("expected String, got {:?}", other),
    }
}

#[test]
fn text_output_empty_stdout_returns_none() {
    let provider = ScriptProvider::new("empty", base_cfg("true", "text"));
    assert!(provider.execute(None).is_none());
}

#[test]
fn text_output_preserves_multiline_content() {
    // Multi-line stdout is preserved as-is in the value string (except trailing whitespace).
    let provider = ScriptProvider::new("multi", base_cfg("printf 'line1\\nline2'", "text"));
    let result = provider.execute(None).expect("text output should not be None");
    match result.get("value").unwrap() {
        Value::String(s) => assert_eq!(s, "line1\nline2"),
        other => panic!("expected String, got {:?}", other),
    }
}
