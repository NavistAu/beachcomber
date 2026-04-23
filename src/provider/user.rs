use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};

pub struct UserProvider;

impl Provider for UserProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "user".to_string(),
            fields: vec![
                FieldSchema {
                    name: "name".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "uid".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::Global,
                },
            ],
            invalidation: InvalidationStrategy::Once,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
        let uid = unsafe { libc::getuid() } as i64;
        let name = get_username(uid as u32);
        let mut result = ProviderResult::new();
        result.insert("name", Value::String(name));
        result.insert("uid", Value::Int(uid));
        vec![(None, result)]
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
