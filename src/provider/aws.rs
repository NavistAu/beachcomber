use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};

pub struct AwsProvider;

impl Provider for AwsProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "aws".to_string(),
            fields: vec![
                FieldSchema { name: "profile".to_string(), field_type: FieldType::String },
                FieldSchema { name: "region".to_string(), field_type: FieldType::String },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                floor_secs: 10,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let profile = std::env::var("AWS_PROFILE").unwrap_or_default();
        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_default();

        // Only return data if at least one value is set
        if profile.is_empty() && region.is_empty() {
            return None;
        }

        let mut result = ProviderResult::new();
        result.insert("profile", Value::String(profile));
        result.insert("region", Value::String(region));
        Some(result)
    }
}
