use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
};

pub struct HostnameProvider;

impl Provider for HostnameProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "hostname".into(),
            sources: vec![host_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(HostnameHost)]
    }
}

fn host_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "host".into(),
        fields: vec![
            FieldSchema {
                name: "name".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "short".into(),
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
        budget_execute: crate::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE,
    }
}

struct HostnameHost;

impl Source for HostnameHost {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(host_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        let full = gethostname();
        let short = full.split('.').next().unwrap_or(&full).to_string();
        let mut result = SourceResult::new();
        result.insert("name", Value::String(full));
        result.insert("short", Value::String(short));
        Ok(result)
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
