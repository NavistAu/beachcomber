use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};

pub struct UptimeProvider;

impl Provider for UptimeProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "uptime".to_string(),
            fields: vec![
                FieldSchema { name: "seconds".to_string(), field_type: FieldType::Int },
                FieldSchema { name: "days".to_string(), field_type: FieldType::Int },
                FieldSchema { name: "hours".to_string(), field_type: FieldType::Int },
                FieldSchema { name: "minutes".to_string(), field_type: FieldType::Int },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                floor_secs: 10,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let mut boottime = libc::timeval { tv_sec: 0, tv_usec: 0 };
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
            return None;
        }

        let now = unsafe { libc::time(std::ptr::null_mut()) };
        let uptime_secs = now - boottime.tv_sec;

        let days = uptime_secs / 86400;
        let hours = (uptime_secs % 86400) / 3600;
        let minutes = (uptime_secs % 3600) / 60;

        let mut result = ProviderResult::new();
        result.insert("seconds", Value::Int(uptime_secs));
        result.insert("days", Value::Int(days));
        result.insert("hours", Value::Int(hours));
        result.insert("minutes", Value::Int(minutes));
        Some(result)
    }
}
