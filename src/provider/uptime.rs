use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};

pub struct UptimeProvider;

impl Provider for UptimeProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "uptime".into(),
            sources: vec![time_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(UptimeTime)]
    }
}

fn time_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "time".into(),
        fields: vec![
            FieldSchema {
                name: "seconds".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "days".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "hours".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "minutes".into(),
                field_type: FieldType::Int,
            },
        ],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 60 },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
    }
}

struct UptimeTime;

impl Source for UptimeTime {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(time_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let mut boottime = libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        };
        let mut size = std::mem::size_of::<libc::timeval>();
        let mut mib = [libc::CTL_KERN, libc::KERN_BOOTTIME];

        let ret = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                2,
                &mut boottime as *mut _ as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };

        if ret != 0 {
            return SourceResult::new();
        }

        let now = unsafe { libc::time(std::ptr::null_mut()) };
        let uptime_secs = now - boottime.tv_sec;

        let days = uptime_secs / 86400;
        let hours = (uptime_secs % 86400) / 3600;
        let minutes = (uptime_secs % 3600) / 60;

        let mut result = SourceResult::new();
        result.insert("seconds", Value::Int(uptime_secs));
        result.insert("days", Value::Int(days));
        result.insert("hours", Value::Int(hours));
        result.insert("minutes", Value::Int(minutes));
        result
    }
}
