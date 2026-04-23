use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};

pub struct CondaProvider;

impl Provider for CondaProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "conda".to_string(),
            fields: vec![FieldSchema {
                name: "env".to_string(),
                field_type: FieldType::String,
                scope: FieldScope::Global,
            }],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 30,
                floor_secs: 10,
            },
        }
    }

    fn execute(&self, _path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
        let Some(env) = std::env::var("CONDA_DEFAULT_ENV")
            .ok()
            .filter(|s| !s.is_empty())
        else {
            return Vec::new();
        };

        let mut result = ProviderResult::new();
        result.insert("env", Value::String(env));
        vec![(None, result)]
    }
}
