use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
};

pub struct UnameProvider;

impl Provider for UnameProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "uname".into(),
            sources: vec![system_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(UnameSystem)]
    }
}

fn system_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "system".into(),
        fields: vec![
            FieldSchema {
                name: "sysname".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "release".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "version".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "machine".into(),
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
        fsevents_reinstate: false,
        decay: None,
        budget: crate::scheduler::lifecycle::DEFAULT_BUDGET,
        timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
    }
}

struct UnameSystem;

impl Source for UnameSystem {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(system_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        let Some(info) = crate::plugin::sys::uname() else {
            return Err("uname failed".into());
        };
        let mut result = SourceResult::new();
        result.insert("sysname", Value::String(info.sysname));
        result.insert("release", Value::String(info.release));
        result.insert("version", Value::String(info.version));
        result.insert("machine", Value::String(info.machine));
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::UnameProvider;
    use crate::provider::{InvalidationStrategy, Provider};

    #[test]
    fn uname_provider_metadata() {
        let p = UnameProvider;
        let meta = p.metadata();
        assert_eq!(meta.name, "uname");
        assert_eq!(meta.sources.len(), 1);
        let src = &meta.sources[0];
        assert_eq!(src.name, "system");
        assert!(
            matches!(src.invalidation, InvalidationStrategy::Watch { .. }),
            "uname system source should use Watch invalidation"
        );
        let field_names: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(field_names.contains(&"sysname"));
        assert!(field_names.contains(&"release"));
        assert!(field_names.contains(&"version"));
        assert!(field_names.contains(&"machine"));
    }

    #[test]
    fn uname_provider_executes() {
        let p = UnameProvider;
        let sources = p.sources();
        let result = sources[0].execute(None).unwrap();
        let sysname = result
            .fields
            .get("sysname")
            .expect("should have sysname field");
        let sysname_text = sysname.as_text();
        assert!(
            sysname_text == "Darwin" || sysname_text == "Linux",
            "sysname should be Darwin or Linux, got: {}",
            sysname_text
        );

        let machine = result
            .fields
            .get("machine")
            .expect("should have machine field");
        let machine_text = machine.as_text();
        assert!(!machine_text.is_empty(), "machine should not be empty");

        let release = result
            .fields
            .get("release")
            .expect("should have release field");
        assert!(!release.as_text().is_empty(), "release should not be empty");

        let version = result
            .fields
            .get("version")
            .expect("should have version field");
        assert!(!version.as_text().is_empty(), "version should not be empty");
    }
}
