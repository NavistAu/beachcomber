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
    // getpwuid is not thread-safe (global static buffer). Use getpwuid_r with a
    // caller-supplied buffer. Buffer size 1024 is sufficient for most systems;
    // retry loop doubles on ERANGE (POSIX-compliant).
    // std::mem::zeroed() is used for the passwd struct — the struct layout differs
    // between macOS (has pw_change, pw_class, pw_expire) and Linux, so a struct
    // literal would require #[cfg] guards. zeroed() is portable; getpwuid_r fills
    // the fields it uses.
    let mut buf = vec![0i8; 1024];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();

    loop {
        let ret =
            unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
        if ret == libc::ERANGE {
            // Buffer too small; double and retry.
            let new_len = buf.len() * 2;
            buf.resize(new_len, 0);
            continue;
        }
        break;
    }

    if result.is_null() {
        return format!("{uid}");
    }
    let name = unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) };
    name.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::get_username;

    #[test]
    fn get_username_concurrent_no_panic() {
        // Call get_username from 8 threads simultaneously.
        // With getpwuid this would race on the global buffer; with getpwuid_r each
        // thread has its own buffer so this is safe.
        let uid = unsafe { libc::getuid() };
        let handles: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(move || get_username(uid)))
            .collect();
        let results: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        // All results must be identical (same uid → same name).
        let first = &results[0];
        for r in &results {
            assert_eq!(
                r, first,
                "concurrent get_username returned different values"
            );
        }
    }
}
