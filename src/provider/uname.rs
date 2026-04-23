use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};

pub struct UnameProvider;

impl Provider for UnameProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "uname".to_string(),
            fields: vec![
                FieldSchema {
                    name: "sysname".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "release".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "version".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "machine".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
            ],
            invalidation: InvalidationStrategy::Once,
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let info = uname_info()?;
        let mut result = ProviderResult::new();
        result.insert("sysname", Value::String(info.sysname));
        result.insert("release", Value::String(info.release));
        result.insert("version", Value::String(info.version));
        result.insert("machine", Value::String(info.machine));
        Some(result)
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
