use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};

pub struct LoadProvider;

impl Provider for LoadProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "load".into(),
            sources: vec![loadavg_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(LoadAvg)]
    }
}

fn loadavg_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "loadavg".into(),
        fields: vec![
            FieldSchema {
                name: "one".into(),
                field_type: FieldType::Float,
            },
            FieldSchema {
                name: "five".into(),
                field_type: FieldType::Float,
            },
            FieldSchema {
                name: "fifteen".into(),
                field_type: FieldType::Float,
            },
        ],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 10 },
        keep_alive: KeepAlive::Polls(6),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
    }
}

struct LoadAvg;

impl Source for LoadAvg {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(loadavg_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let mut loadavg: [f64; 3] = [0.0; 3];
        let ret = unsafe { libc::getloadavg(loadavg.as_mut_ptr(), 3) };
        if ret < 0 {
            return SourceResult::new();
        }
        let mut result = SourceResult::new();
        result.insert("one", Value::Float(loadavg[0]));
        result.insert("five", Value::Float(loadavg[1]));
        result.insert("fifteen", Value::Float(loadavg[2]));
        result
    }
}
