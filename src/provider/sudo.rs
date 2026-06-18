use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::path::Path;
use std::time::{Duration, SystemTime};

pub struct SudoProvider;

/// Default sudo timeout (5 minutes).
/// LIMITATION: the actual timestamp_timeout from sudoers may differ. Reading
/// sudoers requires privilege and is out of S6 scope.
const SUDO_TIMEOUT: Duration = Duration::from_secs(300);

impl Provider for SudoProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "sudo".into(),
            sources: vec![state_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(SudoState)]
    }
}

fn state_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "state".into(),
        fields: vec![FieldSchema {
            name: "active".into(),
            field_type: FieldType::Bool,
        }],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 30 },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
    }
}

struct SudoState;

impl Source for SudoState {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(state_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let mut result = SourceResult::new();
        // Known limitations (not fixable without privilege or helper binary):
        // - timestamp_timeout: hardcoded to 5 minutes; actual sudoers Defaults may differ.
        // - tty_tickets: default in modern sudo; each TTY has its own ticket.
        //   We read a single dir/file without TTY awareness → potential false positives
        //   (other TTY's ticket still fresh) or false negatives (this TTY expired).
        if let Some(active) = has_active_sudo() {
            result.insert("active", Value::Bool(active));
        }
        // If None: timestamp file unreadable (likely root-only); omit field rather than lie.
        result
    }
}

/// Returns Some(true/false) if the timestamp state is readable, None if not
/// (permission denied or does not exist — in both cases we cannot distinguish
/// "no active sudo" from "root-only unreadable").
#[cfg(target_os = "macos")]
fn has_active_sudo() -> Option<bool> {
    let user = std::env::var("USER").ok().filter(|s| !s.is_empty())?;
    let dir = Path::new("/var/db/sudo").join(&user);
    // check_timestamp_dir_opt returns None on read_dir failure (permission
    // denied / not exists) — propagate None rather than collapsing to false.
    check_timestamp_dir_opt(&dir)
}

#[cfg(target_os = "linux")]
fn has_active_sudo() -> Option<bool> {
    let user = std::env::var("USER").ok().filter(|s| !s.is_empty())?;
    let path = Path::new("/var/run/sudo/ts").join(&user);
    if path.is_file() {
        return Some(check_file_recent(&path));
    }
    let path = Path::new("/run/sudo/ts").join(&user);
    if path.is_file() {
        return Some(check_file_recent(&path));
    }
    // File not found → unknown (could be root-only dir, or no recent sudo).
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn has_active_sudo() -> Option<bool> {
    None
}

/// Tri-state directory check: Some(true) if any entry fresh, Some(false) if all
/// stale or dir empty, None if dir unreadable (permission denied / not exists).
#[cfg(target_os = "macos")]
fn check_timestamp_dir_opt(dir: &Path) -> Option<bool> {
    let entries = std::fs::read_dir(dir).ok()?; // None on permission error
    let mut any_fresh = false;
    for entry in entries.flatten() {
        if check_file_recent(&entry.path()) {
            any_fresh = true;
        }
    }
    Some(any_fresh)
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

/// Testable entry point: given a path (dir on macOS-style, file on Linux-style),
/// return a SourceResult with `active` field set if the state is determinable.
/// Unreadable / non-existent paths produce an empty result (no `active` field).
#[cfg(any(test, feature = "test-helpers"))]
pub fn sudo_active_with_ts_path(path: &Path) -> SourceResult {
    let mut result = SourceResult::new();
    if path.is_dir() {
        // macOS-style: directory of timestamp files
        match std::fs::read_dir(path) {
            Ok(entries) => {
                let fresh = entries.flatten().any(|e| check_file_recent(&e.path()));
                result.insert("active", Value::Bool(fresh));
            }
            Err(_) => {} // unreadable → omit
        }
    } else if path.is_file() {
        result.insert("active", Value::Bool(check_file_recent(path)));
    }
    // Does not exist / not readable → empty result (omit active field)
    result
}
