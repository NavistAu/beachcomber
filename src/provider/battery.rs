use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata, ProviderResult, Value,
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
                },
                FieldSchema {
                    name: "charging".to_string(),
                    field_type: FieldType::Bool,
                },
                FieldSchema {
                    name: "time_remaining".to_string(),
                    field_type: FieldType::String,
                },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 30,
                floor_secs: 5,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let output = Command::new("pmset").args(["-g", "batt"]).output().ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_pmset_output(&stdout)
    }
}

fn parse_pmset_output(output: &str) -> Option<ProviderResult> {
    // Example: " -InternalBattery-0 (id=...)	85%; charging; 1:23 remaining present: true"
    let mut percent: i64 = 0;
    let mut charging = false;
    let mut time_remaining = "unknown".to_string();

    for line in output.lines() {
        let line = line.trim();
        // Find the line with percentage
        if let Some(pct_pos) = line.find('%') {
            // Walk backwards from % to find the number
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

            if line.contains("remaining") {
                // Extract time like "1:23 remaining"
                if let Some(rem_pos) = line.find("remaining") {
                    let before_rem = line[..rem_pos].trim();
                    if let Some(time) = before_rem.rsplit(';').next() {
                        time_remaining = time.trim().to_string();
                    }
                }
            } else if line.contains("(no estimate)") {
                time_remaining = "calculating".to_string();
            } else if line.contains("charged") {
                time_remaining = "full".to_string();
            }
        }
    }

    // If we never found a percentage, there's probably no battery
    if percent == 0 && !output.contains('%') {
        return None;
    }

    let mut result = ProviderResult::new();
    result.insert("percent", Value::Int(percent));
    result.insert("charging", Value::Bool(charging));
    result.insert("time_remaining", Value::String(time_remaining));
    Some(result)
}
