use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::process::Command;

pub struct OpProvider;

impl Provider for OpProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "op".to_string(),
            fields: vec![
                FieldSchema {
                    name: "signed_in".to_string(),
                    field_type: FieldType::Bool,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "account".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                floor_secs: 30,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let mut result = ProviderResult::new();

        // Check for service account token first (non-interactive).
        if std::env::var("OP_SERVICE_ACCOUNT_TOKEN").is_ok() {
            result.insert("signed_in", Value::Bool(true));
            result.insert("account", Value::String("service-account".to_string()));
            return Some(result);
        }

        // Check if op CLI is available and authenticated.
        let output = Command::new("op")
            .args(["whoami", "--format=json"])
            .output();

        match output {
            Ok(out) if out.status.success() => {
                result.insert("signed_in", Value::Bool(true));
                let account = parse_account(&out.stdout);
                result.insert("account", Value::String(account));
            }
            _ => {
                result.insert("signed_in", Value::Bool(false));
                result.insert("account", Value::String(String::new()));
            }
        }

        Some(result)
    }
}

fn parse_account(stdout: &[u8]) -> String {
    let Ok(text) = std::str::from_utf8(stdout) else {
        return String::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return String::new();
    };
    // op whoami --format=json returns {"account_uuid":"...","email":"...","url":"..."}
    json.get("email")
        .or(json.get("url"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}
