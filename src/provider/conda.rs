use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};

pub struct CondaProvider;

impl Provider for CondaProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "conda".into(),
            sources: vec![env_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(CondaEnv)]
    }
}

fn env_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "env".into(),
        fields: vec![FieldSchema {
            name: "env".into(),
            field_type: FieldType::String,
        }],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 30 },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
    }
}

struct CondaEnv;

impl Source for CondaEnv {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(env_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let Some(env) = std::env::var("CONDA_DEFAULT_ENV")
            .ok()
            .filter(|s| !s.is_empty())
        else {
            return SourceResult::new();
        };

        let mut result = SourceResult::new();
        result.insert("env", Value::String(env));
        result
    }
}
