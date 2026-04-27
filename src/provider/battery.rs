use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::process::Command;
use std::sync::OnceLock;

pub struct BatteryProvider;

// ── macOS: pmset — single source ─────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn state_meta() -> SourceMetadata {
    SourceMetadata {
        name: "state".into(),
        fields: vec![
            FieldSchema {
                name: "percent".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "charging".into(),
                field_type: FieldType::Bool,
            },
            FieldSchema {
                name: "status".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "time_remaining_secs".into(),
                field_type: FieldType::Int,
            },
        ],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 30 },
        keep_alive: KeepAlive::Polls(4),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: false,
    }
}

#[cfg(target_os = "macos")]
struct BatteryState;

#[cfg(target_os = "macos")]
impl Source for BatteryState {
    fn metadata(&self) -> &SourceMetadata {
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(state_meta)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let Ok(output) = Command::new("pmset").args(["-g", "batt"]).output() else {
            return SourceResult::new();
        };
        if !output.status.success() {
            return SourceResult::new();
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_pmset_output_to_source(&stdout)
    }
}

#[cfg(target_os = "macos")]
fn parse_pmset_output_to_source(output: &str) -> SourceResult {
    let mut percent: i64 = 0;
    let mut charging = false;
    let mut time_remaining_secs: i64 = 0;
    let mut status: String = "unknown".to_string();
    let mut found = false;

    for line in output.lines() {
        let line = line.trim();
        if let Some(pct_pos) = line.find('%') {
            found = true;
            let before = &line[..pct_pos];
            let num_str: String = before
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            percent = num_str.parse().unwrap_or(0);

            charging = line.contains("charging")
                && !line.contains("discharging")
                && !line.contains("not charging");
            let discharging = line.contains("discharging");

            if line.contains("(no estimate)") {
                status = "calculating".into();
                time_remaining_secs = 0;
            } else if line.contains("charged") {
                status = "charged".into();
                time_remaining_secs = 0;
            } else if line.contains("remaining") {
                if let Some(rem_pos) = line.find("remaining") {
                    let before_rem = line[..rem_pos].trim();
                    if let Some(time_str) = before_rem.rsplit(';').next() {
                        let time_str = time_str.trim();
                        if let Some((h, m)) = time_str.split_once(':') {
                            let hours: i64 = h.trim().parse().unwrap_or(0);
                            let mins: i64 = m.trim().parse().unwrap_or(0);
                            time_remaining_secs = hours * 3600 + mins * 60;
                        }
                    }
                }
                status = if charging {
                    "charging".into()
                } else {
                    "discharging".into()
                };
            } else if charging {
                status = "charging".into();
            } else if discharging {
                status = "discharging".into();
            }
        }
    }

    if !found {
        return SourceResult::new();
    }

    let mut result = SourceResult::new();
    result.insert("percent", Value::Int(percent));
    result.insert("charging", Value::Bool(charging));
    result.insert("time_remaining_secs", Value::Int(time_remaining_secs));
    result.insert("status", Value::String(status));
    result
}

/// Expose the pmset parser for integration tests.
#[cfg(target_os = "macos")]
pub fn parse_pmset_output(output: &str) -> Option<crate::provider::ProviderResult> {
    use crate::provider::ProviderResult;
    let sr = parse_pmset_output_to_source(output);
    if sr.fields.is_empty() {
        return None;
    }
    let mut result = ProviderResult::new();
    for (k, v) in sr.fields {
        result.insert(k, v);
    }
    Some(result)
}

// ── Linux: sysfs + UPower — two sources ──────────────────────────────────────

#[cfg(target_os = "linux")]
fn level_meta() -> SourceMetadata {
    SourceMetadata {
        name: "level".into(),
        fields: vec![
            FieldSchema {
                name: "percent".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "charging".into(),
                field_type: FieldType::Bool,
            },
            FieldSchema {
                name: "status_raw".into(),
                field_type: FieldType::String,
            },
        ],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 30 },
        keep_alive: KeepAlive::Polls(4),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: false,
    }
}

#[cfg(target_os = "linux")]
fn upower_meta() -> SourceMetadata {
    SourceMetadata {
        name: "upower".into(),
        fields: vec![
            FieldSchema {
                name: "time_remaining_secs".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "status".into(),
                field_type: FieldType::String,
            },
        ],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 60 },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: false,
    }
}

#[cfg(target_os = "linux")]
struct BatteryLevel;

#[cfg(target_os = "linux")]
impl Source for BatteryLevel {
    fn metadata(&self) -> &SourceMetadata {
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(level_meta)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let Some(battery_dir) = find_battery_dir() else {
            return SourceResult::new();
        };
        let Ok(capacity_str) = std::fs::read_to_string(battery_dir.join("capacity")) else {
            return SourceResult::new();
        };
        let Ok(percent) = capacity_str.trim().parse::<i64>() else {
            return SourceResult::new();
        };
        let Ok(status_str) = std::fs::read_to_string(battery_dir.join("status")) else {
            return SourceResult::new();
        };
        let sysfs_status = status_str.trim().to_string();
        let charging = sysfs_status == "Charging";

        let mut result = SourceResult::new();
        result.insert("percent", Value::Int(percent));
        result.insert("charging", Value::Bool(charging));
        result.insert("status_raw", Value::String(sysfs_status));
        result
    }
}

#[cfg(target_os = "linux")]
struct BatteryUpower;

#[cfg(target_os = "linux")]
impl Source for BatteryUpower {
    fn metadata(&self) -> &SourceMetadata {
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(upower_meta)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        // Need the sysfs status to compute the human-readable status.
        let sysfs_status = find_battery_dir()
            .and_then(|d| std::fs::read_to_string(d.join("status")).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        let time_remaining_secs = get_upower_time_remaining_secs().unwrap_or(0);

        let status = if sysfs_status == "Full" {
            "charged".to_string()
        } else if sysfs_status == "Charging" {
            if time_remaining_secs > 0 {
                "charging".to_string()
            } else {
                "calculating".to_string()
            }
        } else if sysfs_status == "Discharging" {
            if time_remaining_secs > 0 {
                "discharging".to_string()
            } else {
                "calculating".to_string()
            }
        } else {
            "unknown".to_string()
        };

        let mut result = SourceResult::new();
        result.insert("time_remaining_secs", Value::Int(time_remaining_secs));
        result.insert("status", Value::String(status));
        result
    }
}

#[cfg(target_os = "linux")]
fn find_battery_dir() -> Option<std::path::PathBuf> {
    let power_supply = std::path::Path::new("/sys/class/power_supply");
    for entry in std::fs::read_dir(power_supply).ok()? {
        let entry = entry.ok()?;
        let type_path = entry.path().join("type");
        if let Ok(contents) = std::fs::read_to_string(&type_path)
            && contents.trim() == "Battery"
        {
            return Some(entry.path());
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn get_upower_time_remaining_secs() -> Option<i64> {
    let output = Command::new("upower").args(["-e"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let battery_path = stdout.lines().find(|l| l.contains("battery"))?;

    let info = Command::new("upower")
        .args(["-i", battery_path.trim()])
        .output()
        .ok()?;
    if !info.status.success() {
        return None;
    }
    let info_str = String::from_utf8_lossy(&info.stdout);
    for line in info_str.lines() {
        let line = line.trim();
        if line.starts_with("time to empty:") || line.starts_with("time to full:") {
            let val_str = line.splitn(2, ':').nth(1)?.trim().to_string();
            if val_str.contains("hour") {
                let hours: f64 = val_str.split_whitespace().next()?.parse().ok()?;
                return Some((hours * 3600.0) as i64);
            } else if val_str.contains("minute") {
                let mins: f64 = val_str.split_whitespace().next()?.parse().ok()?;
                return Some((mins * 60.0) as i64);
            } else if val_str.contains("second") {
                let secs: f64 = val_str.split_whitespace().next()?.parse().ok()?;
                return Some(secs as i64);
            }
            return None;
        }
    }
    None
}

// ── Provider ──────────────────────────────────────────────────────────────────

impl Provider for BatteryProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "battery".into(),
            #[cfg(target_os = "macos")]
            sources: vec![state_meta()],
            #[cfg(target_os = "linux")]
            sources: vec![level_meta(), upower_meta()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        #[cfg(target_os = "macos")]
        return vec![Box::new(BatteryState)];
        #[cfg(target_os = "linux")]
        return vec![Box::new(BatteryLevel), Box::new(BatteryUpower)];
    }
}
