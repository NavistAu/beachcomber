use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata, ProviderResult, Value,
};
use std::path::Path;
use std::time::{Duration, SystemTime};

pub struct SudoProvider;

/// Default sudo timeout (5 minutes).
const SUDO_TIMEOUT: Duration = Duration::from_secs(300);

impl Provider for SudoProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "sudo".to_string(),
            fields: vec![FieldSchema {
                name: "active".to_string(),
                field_type: FieldType::Bool,
            }],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 30,
                floor_secs: 10,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let active = has_active_sudo();
        let mut result = ProviderResult::new();
        result.insert("active", Value::Bool(active));
        Some(result)
    }
}

#[cfg(target_os = "macos")]
fn has_active_sudo() -> bool {
    // macOS stores sudo timestamps in /var/db/sudo/<user>/
    let user = std::env::var("USER").unwrap_or_default();
    if user.is_empty() {
        return false;
    }
    let dir = Path::new("/var/db/sudo").join(&user);
    check_timestamp_dir(&dir)
}

#[cfg(target_os = "linux")]
fn has_active_sudo() -> bool {
    // Linux stores sudo timestamps in /var/run/sudo/ts/<user>
    let user = std::env::var("USER").unwrap_or_default();
    if user.is_empty() {
        return false;
    }
    let path = Path::new("/var/run/sudo/ts").join(&user);
    if path.is_file() {
        return check_file_recent(&path);
    }
    // Some distros use /run/sudo/ts/
    let path = Path::new("/run/sudo/ts").join(&user);
    if path.is_file() {
        return check_file_recent(&path);
    }
    false
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn has_active_sudo() -> bool {
    false
}

/// Check if any timestamp file in a directory was modified within the sudo timeout.
#[cfg(target_os = "macos")]
fn check_timestamp_dir(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        if check_file_recent(&entry.path()) {
            return true;
        }
    }
    false
}

/// Check if a file's mtime is within the sudo timeout window.
fn check_file_recent(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let Ok(elapsed) = SystemTime::now().duration_since(modified) else {
        return false;
    };
    elapsed < SUDO_TIMEOUT
}
