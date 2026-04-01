use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata, ProviderResult, Value,
};

pub struct LoadProvider;

impl Provider for LoadProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "load".to_string(),
            fields: vec![
                FieldSchema {
                    name: "one".to_string(),
                    field_type: FieldType::Float,
                },
                FieldSchema {
                    name: "five".to_string(),
                    field_type: FieldType::Float,
                },
                FieldSchema {
                    name: "fifteen".to_string(),
                    field_type: FieldType::Float,
                },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 10,
                floor_secs: 5,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let mut loadavg: [f64; 3] = [0.0; 3];
        let ret = unsafe { libc::getloadavg(loadavg.as_mut_ptr(), 3) };
        if ret < 0 {
            return None;
        }

        let mut result = ProviderResult::new();
        result.insert("one", Value::Float(loadavg[0]));
        result.insert("five", Value::Float(loadavg[1]));
        result.insert("fifteen", Value::Float(loadavg[2]));
        Some(result)
    }
}
