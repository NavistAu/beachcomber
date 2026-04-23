use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};

pub struct HostnameProvider;

impl Provider for HostnameProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "hostname".to_string(),
            fields: vec![
                FieldSchema {
                    name: "name".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "short".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
            ],
            invalidation: InvalidationStrategy::Once,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
        let full = gethostname();
        let short = full.split('.').next().unwrap_or(&full).to_string();
        let mut result = ProviderResult::new();
        result.insert("name", Value::String(full));
        result.insert("short", Value::String(short));
        vec![(None, result)]
    }
}

fn gethostname() -> String {
    let mut buf = vec![0u8; 256];
    let ret = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if ret != 0 {
        return "unknown".to_string();
    }
    let nul_pos = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..nul_pos]).to_string()
}
