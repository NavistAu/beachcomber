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
