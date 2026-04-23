use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::process::Command;

pub struct BatteryProvider;

impl Provider for BatteryProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "battery".to_string(),
            fields: vec![
                FieldSchema {
                    name: "percent".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "charging".to_string(),
                    field_type: FieldType::Bool,
                    scope: FieldScope::Global,
                },
                // time_remaining_secs uses 0 as a sentinel for "not applicable" —
                // i.e. the battery is charged or the platform can't compute an estimate.
                // Callers should read `status` to disambiguate.
                FieldSchema {
                    name: "time_remaining_secs".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "status".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 30,
                floor_secs: 5,
            },
        }
    }

    fn execute(&self, _path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
        match execute_platform(_path) {
            Some(result) => vec![(None, result)],
            None => Vec::new(),
        }
    }
}

// === macOS: pmset ===
#[cfg(target_os = "macos")]
fn execute_platform(_path: Option<&str>) -> Option<ProviderResult> {
    let output = Command::new("pmset").args(["-g", "batt"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_pmset_output(&stdout)
}

#[cfg(target_os = "macos")]
pub fn parse_pmset_output(output: &str) -> Option<ProviderResult> {
    let mut percent: i64 = 0;
    let mut charging = false;
    let mut time_remaining_secs: i64 = 0;
    let mut status: String = "unknown".to_string();

    for line in output.lines() {
        let line = line.trim();
        if let Some(pct_pos) = line.find('%') {
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

    if percent == 0 && !output.contains('%') {
        return None;
    }

    let mut result = ProviderResult::new();
    result.insert("percent", Value::Int(percent));
    result.insert("charging", Value::Bool(charging));
    result.insert("time_remaining_secs", Value::Int(time_remaining_secs));
    result.insert("status", Value::String(status));
    Some(result)
}

// === Linux: sysfs + UPower ===
#[cfg(target_os = "linux")]
fn execute_platform(_path: Option<&str>) -> Option<ProviderResult> {
    let battery_dir = find_battery_dir()?;
    let capacity_str = std::fs::read_to_string(battery_dir.join("capacity")).ok()?;
    let percent: i64 = capacity_str.trim().parse().ok()?;

    let status_str = std::fs::read_to_string(battery_dir.join("status")).ok()?;
    let sysfs_status = status_str.trim();
    let charging = sysfs_status == "Charging";

    let (time_remaining_secs, status) = if sysfs_status == "Full" {
        (0i64, "charged".to_string())
    } else if sysfs_status == "Charging" {
        let secs = get_upower_time_remaining_secs().unwrap_or(0);
        let st = if secs > 0 {
            "charging".to_string()
        } else {
            "calculating".to_string()
        };
        (secs, st)
    } else if sysfs_status == "Discharging" {
        let secs = get_upower_time_remaining_secs().unwrap_or(0);
        let st = if secs > 0 {
            "discharging".to_string()
        } else {
            "calculating".to_string()
        };
        (secs, st)
    } else {
        (0i64, "unknown".to_string())
    };

    let mut result = ProviderResult::new();
    result.insert("percent", Value::Int(percent));
    result.insert("charging", Value::Bool(charging));
    result.insert("time_remaining_secs", Value::Int(time_remaining_secs));
    result.insert("status", Value::String(status));
    Some(result)
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
            // UPower reports time as "X.X hours" or "X minutes" etc.
            // Parse into seconds as best we can.
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
