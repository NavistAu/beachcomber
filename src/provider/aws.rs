use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata, ProviderResult, Value,
};

pub struct AwsProvider;

impl Provider for AwsProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "aws".to_string(),
            fields: vec![
                FieldSchema {
                    name: "profile".to_string(),
                    field_type: FieldType::String,
                },
                FieldSchema {
                    name: "region".to_string(),
                    field_type: FieldType::String,
                },
                FieldSchema {
                    name: "source".to_string(),
                    field_type: FieldType::String,
                },
                FieldSchema {
                    name: "expiration".to_string(),
                    field_type: FieldType::String,
                },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                floor_secs: 10,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        // Profile detection: AWS_PROFILE (native, granted) -> AWS_VAULT (aws-vault)
        let (profile, source) = if let Ok(p) = std::env::var("AWS_PROFILE") {
            if !p.is_empty() {
                (p, "profile")
            } else {
                (String::new(), "")
            }
        } else if let Ok(v) = std::env::var("AWS_VAULT") {
            if !v.is_empty() {
                (v, "vault")
            } else {
                (String::new(), "")
            }
        } else {
            (String::new(), "")
        };

        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_default();

        // Expiration: AWS_CREDENTIAL_EXPIRATION (aws-vault, granted) -> AWS_SESSION_EXPIRATION (granted)
        let expiration = std::env::var("AWS_CREDENTIAL_EXPIRATION")
            .or_else(|_| std::env::var("AWS_SESSION_EXPIRATION"))
            .unwrap_or_default();

        if profile.is_empty() && region.is_empty() {
            return None;
        }

        let mut result = ProviderResult::new();
        result.insert("profile", Value::String(profile));
        result.insert("region", Value::String(region));
        if !source.is_empty() {
            result.insert("source", Value::String(source.to_string()));
        }
        if !expiration.is_empty() {
            result.insert("expiration", Value::String(expiration));
        }
        Some(result)
    }
}
