use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};

pub struct AwsProvider;

impl Provider for AwsProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "aws".into(),
            sources: vec![profile_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(AwsProfile)]
    }
}

fn profile_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "profile".into(),
        fields: vec![
            FieldSchema {
                name: "profile".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "region".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "source".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "expiration".into(),
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

struct AwsProfile;

impl Source for AwsProfile {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(profile_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
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
            return SourceResult::new();
        }

        let mut result = SourceResult::new();
        result.insert("profile", Value::String(profile));
        result.insert("region", Value::String(region));
        if !source.is_empty() {
            result.insert("source", Value::String(source.to_string()));
        }
        if !expiration.is_empty() {
            result.insert("expiration", Value::String(expiration));
        }
        result
    }
}
