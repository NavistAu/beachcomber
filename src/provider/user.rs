use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
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

struct UserName;

impl Source for UserName {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(name_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let uid = unsafe { libc::getuid() } as i64;
        let name = get_username(uid as u32);
        let mut result = SourceResult::new();
        result.insert("name", Value::String(name));
        result.insert("uid", Value::Int(uid));
        result
    }
}

fn get_username(uid: u32) -> String {
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return format!("{uid}");
    }
    let name = unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) };
    name.to_string_lossy().to_string()
}
