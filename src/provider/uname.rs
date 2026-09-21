use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
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
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![],
            abs_paths: vec![],
        },
        keep_alive: KeepAlive::Never,
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: false,
    }
}

struct UnameSystem;

impl Source for UnameSystem {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(system_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let Some(info) = uname_info() else {
            return SourceResult::new();
        };
        let mut result = SourceResult::new();
        result.insert("sysname", Value::String(info.sysname));
        result.insert("release", Value::String(info.release));
        result.insert("version", Value::String(info.version));
        result.insert("machine", Value::String(info.machine));
        result
    }
}

struct UnameInfo {
    sysname: String,
    release: String,
    version: String,
    machine: String,
}

fn cstr_to_string(buf: &[libc::c_char]) -> String {
    let nul_pos = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let bytes: Vec<u8> = buf[..nul_pos].iter().map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).to_string()
}

fn uname_info() -> Option<UnameInfo> {
    unsafe {
        let mut buf: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut buf) != 0 {
            return None;
        }
        Some(UnameInfo {
            sysname: cstr_to_string(&buf.sysname),
            release: cstr_to_string(&buf.release),
            version: cstr_to_string(&buf.version),
            machine: cstr_to_string(&buf.machine),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::UnameProvider;
    use crate::provider::{InvalidationStrategy, Provider, SourceScope};

    #[test]
    fn uname_provider_metadata() {
        let p = UnameProvider;
        let meta = p.metadata();
        assert_eq!(meta.name, "uname");
        assert_eq!(meta.sources.len(), 1);
        let src = &meta.sources[0];
        assert_eq!(src.name, "system");
        assert_eq!(src.scope, SourceScope::Global);
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
        let result = sources[0].execute(None);
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
