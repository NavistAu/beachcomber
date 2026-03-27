use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};

pub struct CondaProvider;

impl Provider for CondaProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "conda".to_string(),
            fields: vec![
                FieldSchema { name: "env".to_string(), field_type: FieldType::String },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 30,
                floor_secs: 10,
            },
            global: false,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let env = std::env::var("CONDA_DEFAULT_ENV").ok().filter(|s| !s.is_empty())?;

        let mut result = ProviderResult::new();
        result.insert("env", Value::String(env));
        Some(result)
    }
}
