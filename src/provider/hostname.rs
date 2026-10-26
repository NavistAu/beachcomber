use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
};

pub struct HostnameProvider;

impl Provider for HostnameProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "hostname".into(),
            sources: vec![host_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(HostnameHost)]
    }
}

fn host_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "host".into(),
        fields: vec![
            FieldSchema {
                name: "name".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "short".into(),
                field_type: FieldType::String,
            },
        ],
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![],
            abs_paths: vec![],
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        decay: None,
        budget: crate::scheduler::lifecycle::DEFAULT_BUDGET,
        timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
    }
}

struct HostnameHost;

impl Source for HostnameHost {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(host_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        let full = crate::plugin::sys::gethostname().unwrap_or_else(|| "unknown".to_string());
        let short = full.split('.').next().unwrap_or(&full).to_string();
        let mut result = SourceResult::new();
        result.insert("name", Value::String(full));
        result.insert("short", Value::String(short));
        Ok(result)
    }
}
