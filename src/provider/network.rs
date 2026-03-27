use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::process::Command;

pub struct NetworkProvider;

impl Provider for NetworkProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "network".to_string(),
            fields: vec![
                FieldSchema { name: "interface".to_string(), field_type: FieldType::String },
                FieldSchema { name: "ip".to_string(), field_type: FieldType::String },
                FieldSchema { name: "vpn_active".to_string(), field_type: FieldType::Bool },
                FieldSchema { name: "vpn_name".to_string(), field_type: FieldType::String },
                FieldSchema { name: "ssid".to_string(), field_type: FieldType::String },
                FieldSchema { name: "online".to_string(), field_type: FieldType::Bool },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 10,
                floor_secs: 5,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let mut result = ProviderResult::new();

        let (iface, ip) = get_primary_interface();
        result.insert("interface", Value::String(iface));
        result.insert("ip", Value::String(ip.clone()));
        result.insert("online", Value::Bool(!ip.is_empty()));

        let (vpn_active, vpn_name) = detect_vpn();
        result.insert("vpn_active", Value::Bool(vpn_active));
        result.insert("vpn_name", Value::String(vpn_name));

        let ssid = get_wifi_ssid();
        result.insert("ssid", Value::String(ssid));

        Some(result)
    }
}

fn get_primary_interface() -> (String, String) {
    // Use route to find the default interface
    let output = Command::new("route")
        .args(["-n", "get", "default"])
        .output()
        .ok();

    let iface = output.as_ref().and_then(|o| {
        let stdout = String::from_utf8_lossy(&o.stdout);
        stdout.lines()
            .find(|l| l.trim().starts_with("interface:"))
            .map(|l| l.trim().strip_prefix("interface:").unwrap_or("").trim().to_string())
    }).unwrap_or_default();

    if iface.is_empty() {
        return (String::new(), String::new());
    }

    // Get IP for that interface
    let output = Command::new("ifconfig")
        .arg(&iface)
        .output()
        .ok();

    let ip = output.as_ref().and_then(|o| {
        let stdout = String::from_utf8_lossy(&o.stdout);
        stdout.lines()
            .find(|l| l.trim().starts_with("inet ") && !l.contains("127.0.0.1"))
            .map(|l| {
                l.trim().split_whitespace().nth(1).unwrap_or("").to_string()
            })
    }).unwrap_or_default();

    (iface, ip)
}

fn detect_vpn() -> (bool, String) {
    // Check for utun interfaces (common for VPNs on macOS)
    let output = Command::new("ifconfig")
        .output()
        .ok();

    if let Some(o) = output {
        let stdout = String::from_utf8_lossy(&o.stdout);
        for line in stdout.lines() {
            if line.starts_with("utun") && line.contains("flags=") && line.contains("UP") {
                let name = line.split(':').next().unwrap_or("").to_string();
                return (true, name);
            }
        }
    }

    (false, String::new())
}

fn get_wifi_ssid() -> String {
    // macOS: use airport command
    let output = Command::new("/System/Library/PrivateFrameworks/Apple80211.framework/Versions/Current/Resources/airport")
        .args(["-I"])
        .output()
        .ok();

    output.and_then(|o| {
        if !o.status.success() { return None; }
        let stdout = String::from_utf8_lossy(&o.stdout);
        stdout.lines()
            .find(|l| l.trim().starts_with("SSID:"))
            .map(|l| l.trim().strip_prefix("SSID:").unwrap_or("").trim().to_string())
    }).unwrap_or_default()
}
