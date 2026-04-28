use crate::boundaries::process::{ProcessExecutor, RealProcessExecutor};
use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::sync::Arc;

pub struct OpProvider;

impl Provider for OpProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "op".into(),
            sources: vec![vault_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(OpVault::new())]
    }
}

/// Construct an `OpVault` source with a custom executor for seam testing.
#[cfg(any(test, feature = "test-helpers"))]
pub fn op_source_with_executor(executor: Arc<dyn ProcessExecutor>) -> Box<dyn Source> {
    Box::new(OpVault::with_executor(executor))
}

fn vault_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "vault".into(),
        fields: vec![
            FieldSchema {
                name: "signed_in".into(),
                field_type: FieldType::Bool,
            },
            FieldSchema {
                name: "account".into(),
                field_type: FieldType::String,
            },
        ],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 60 },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
    }
}

struct OpVault {
    executor: Arc<dyn ProcessExecutor>,
}

impl OpVault {
    fn new() -> Self {
        Self {
            executor: Arc::new(RealProcessExecutor),
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn with_executor(executor: Arc<dyn ProcessExecutor>) -> Self {
        Self { executor }
    }
}

impl Source for OpVault {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(vault_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let mut result = SourceResult::new();

        // Check for service account token first (non-interactive).
        if std::env::var("OP_SERVICE_ACCOUNT_TOKEN").is_ok() {
            result.insert("signed_in", Value::Bool(true));
            result.insert("account", Value::String("service-account".to_string()));
            return result;
        }

        // Check if op CLI is available and authenticated.
        let output = self
            .executor
            .run("op", vec!["whoami".into(), "--format=json".into()]);

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

        result
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
