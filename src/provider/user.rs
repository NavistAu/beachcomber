use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
};

pub struct UserProvider;

impl Provider for UserProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "user".into(),
            sources: vec![name_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(UserName)]
    }
}

fn name_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "name".into(),
        fields: vec![
            FieldSchema {
                name: "name".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "uid".into(),
                field_type: FieldType::Int,
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
        fsevents_reinstate: false,
        decay: None,
        budget: crate::scheduler::lifecycle::DEFAULT_BUDGET,
        timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
    }
}

struct UserName;

impl Source for UserName {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(name_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        let uid = unsafe { libc::getuid() } as i64;
        let name = crate::plugin::sys::username(uid as u32).unwrap_or_else(|| uid.to_string());
        let mut result = SourceResult::new();
        if !name.is_empty() {
            result.insert("name", Value::String(name));
        }
        result.insert("uid", Value::Int(uid));
        Ok(result)
    }
}
