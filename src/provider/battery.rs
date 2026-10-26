use crate::boundaries::process::{ProcessExecutor, RealProcessExecutor};
#[cfg(target_os = "linux")]
use crate::provider::Value;
use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult,
};
use std::sync::{Arc, OnceLock};

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
        invalidation: InvalidationStrategy::Poll {
            interval_secs: 30,
            count: 4,
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        decay: None,
        budget: crate::scheduler::lifecycle::DEFAULT_BUDGET,
        timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
    }
}

#[cfg(target_os = "macos")]
struct BatteryState {
    executor: Arc<dyn ProcessExecutor>,
}

#[cfg(target_os = "macos")]
impl BatteryState {
    fn new() -> Self {
        Self {
            executor: Arc::new(RealProcessExecutor),
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn with_executor(executor: Arc<dyn ProcessExecutor>) -> Self {
        Self { executor }
    }
}

#[cfg(target_os = "macos")]
impl Source for BatteryState {
    fn metadata(&self) -> &SourceMetadata {
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(state_meta)
    }

    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        let output = self
            .executor
            .run("pmset", vec!["-g".into(), "batt".into()])
            .map_err(|e| format!("pmset failed: {e}"))?;
        if !output.status.success() {
            return Err("pmset exited with an error".into());
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(SourceResult::from_json_object(
            &crate::plugin::sys::parse::pmset(&stdout),
        ))
    }
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
        invalidation: InvalidationStrategy::Poll {
            interval_secs: 30,
            count: 4,
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        decay: None,
        budget: crate::scheduler::lifecycle::DEFAULT_BUDGET,
        timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
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
        invalidation: InvalidationStrategy::Poll {
            interval_secs: 60,
            count: 2,
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        decay: None,
        budget: crate::scheduler::lifecycle::DEFAULT_BUDGET,
        timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
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

    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        let Some(battery_dir) = crate::plugin::sys::find_battery_dir() else {
            return Ok(SourceResult::new());
        };
        let capacity_str = std::fs::read_to_string(battery_dir.join("capacity"))
            .map_err(|e| format!("cannot read battery capacity: {e}"))?;
        let percent = capacity_str
            .trim()
            .parse::<i64>()
            .map_err(|e| format!("cannot parse battery capacity: {e}"))?;
        let status_str = std::fs::read_to_string(battery_dir.join("status"))
            .map_err(|e| format!("cannot read battery status: {e}"))?;
        let sysfs_status = status_str.trim().to_string();
        let charging = sysfs_status == "Charging";

        let mut result = SourceResult::new();
        result.insert("percent", Value::Int(percent));
        result.insert("charging", Value::Bool(charging));
        result.insert("status_raw", Value::String(sysfs_status));
        Ok(result)
    }
}

#[cfg(target_os = "linux")]
struct BatteryUpower {
    executor: Arc<dyn ProcessExecutor>,
}

#[cfg(target_os = "linux")]
impl BatteryUpower {
    fn new() -> Self {
        Self {
            executor: Arc::new(RealProcessExecutor),
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn with_executor(executor: Arc<dyn ProcessExecutor>) -> Self {
        Self { executor }
    }
}

#[cfg(target_os = "linux")]
impl Source for BatteryUpower {
    fn metadata(&self) -> &SourceMetadata {
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(upower_meta)
    }

    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        // Need the sysfs status to compute the human-readable status.
        let sysfs_status = crate::plugin::sys::find_battery_dir()
            .and_then(|d| std::fs::read_to_string(d.join("status")).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        // No estimate from upower is no value: absent, never 0.
        let time_remaining_secs =
            get_upower_time_remaining_secs(&*self.executor).filter(|secs| *secs > 0);

        let status = if sysfs_status == "Full" {
            "charged".to_string()
        } else if sysfs_status == "Charging" {
            if time_remaining_secs.is_some() {
                "charging".to_string()
            } else {
                "calculating".to_string()
            }
        } else if sysfs_status == "Discharging" {
            if time_remaining_secs.is_some() {
                "discharging".to_string()
            } else {
                "calculating".to_string()
            }
        } else {
            "unknown".to_string()
        };

        let mut result = SourceResult::new();
        if let Some(secs) = time_remaining_secs {
            result.insert("time_remaining_secs", Value::Int(secs));
        }
        result.insert("status", Value::String(status));
        Ok(result)
    }
}

#[cfg(target_os = "linux")]
fn get_upower_time_remaining_secs(executor: &dyn ProcessExecutor) -> Option<i64> {
    let output = executor.run("upower", vec!["-e".into()]).ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let battery_path = stdout
        .lines()
        .find(|l| l.contains("battery"))?
        .trim()
        .to_string();

    let info = executor
        .run("upower", vec!["-i".into(), battery_path])
        .ok()?;
    if !info.status.success() {
        return None;
    }
    let info_str = String::from_utf8_lossy(&info.stdout);
    for line in info_str.lines() {
        let line = line.trim();
        if line.starts_with("time to empty:") || line.starts_with("time to full:") {
            let val_str = line.split_once(':')?.1.trim().to_string();
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
        return vec![Box::new(BatteryState::new())];
        #[cfg(target_os = "linux")]
        return vec![Box::new(BatteryLevel), Box::new(BatteryUpower::new())];
    }
}

/// Construct the macOS pmset source with a custom executor for seam testing.
#[cfg(all(target_os = "macos", any(test, feature = "test-helpers")))]
pub fn battery_state_source_with_executor(executor: Arc<dyn ProcessExecutor>) -> Box<dyn Source> {
    Box::new(BatteryState::with_executor(executor))
}

/// Construct the Linux upower source with a custom executor for seam testing.
#[cfg(all(target_os = "linux", any(test, feature = "test-helpers")))]
pub fn battery_upower_source_with_executor(executor: Arc<dyn ProcessExecutor>) -> Box<dyn Source> {
    Box::new(BatteryUpower::with_executor(executor))
}
