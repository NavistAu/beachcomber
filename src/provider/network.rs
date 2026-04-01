use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata, ProviderResult, Value,
};
use std::net::Ipv4Addr;
use std::process::Command;

pub struct NetworkProvider;

impl Provider for NetworkProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "network".to_string(),
            fields: vec![
                FieldSchema {
                    name: "interface".to_string(),
                    field_type: FieldType::String,
                },
                FieldSchema {
                    name: "ip".to_string(),
                    field_type: FieldType::String,
                },
                FieldSchema {
                    name: "vpn_active".to_string(),
                    field_type: FieldType::Bool,
                },
                FieldSchema {
                    name: "vpn_name".to_string(),
                    field_type: FieldType::String,
                },
                FieldSchema {
                    name: "ssid".to_string(),
                    field_type: FieldType::String,
                },
                FieldSchema {
                    name: "online".to_string(),
                    field_type: FieldType::Bool,
                },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 10,
                floor_secs: 5,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let (iface, ip, vpn_active, vpn_name) = scan_interfaces();
        let ssid = get_wifi_ssid();

        let mut result = ProviderResult::new();
        result.insert("interface", Value::String(iface));
        result.insert("ip", Value::String(ip.clone()));
        result.insert("online", Value::Bool(!ip.is_empty()));
        result.insert("vpn_active", Value::Bool(vpn_active));
        result.insert("vpn_name", Value::String(vpn_name));
        result.insert("ssid", Value::String(ssid));
        Some(result)
    }
}

/// Single getifaddrs pass: returns (interface, ip, vpn_active, vpn_name).
fn scan_interfaces() -> (String, String, bool, String) {
    let mut ifaddrs: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut ifaddrs) } != 0 {
        return (String::new(), String::new(), false, String::new());
    }

    let mut best_iface = String::new();
    let mut best_ip = String::new();
    let mut vpn_active = false;
    let mut vpn_name = String::new();

    let mut curr = ifaddrs;
    while !curr.is_null() {
        let entry = unsafe { &*curr };
        let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) }
            .to_string_lossy()
            .to_string();
        let flags = entry.ifa_flags;

        let is_up = flags & (libc::IFF_UP as u32) != 0;
        let is_loopback = flags & (libc::IFF_LOOPBACK as u32) != 0;

        if is_up && !is_loopback {
            // Detect VPN via utun interfaces
            if name.starts_with("utun") && !vpn_active {
                vpn_active = true;
                vpn_name = name.clone();
            }

            // Collect primary IPv4 address
            if !entry.ifa_addr.is_null() {
                let family = unsafe { (*entry.ifa_addr).sa_family } as i32;
                if family == libc::AF_INET {
                    let addr = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in) };
                    let ip = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
                    if !ip.is_loopback() && !ip.is_link_local() {
                        // Prefer en0 (primary ethernet/wifi on macOS), otherwise take first
                        if best_iface.is_empty() || name == "en0" {
                            best_iface = name.clone();
                            best_ip = ip.to_string();
                        }
                    }
                }
            }
        }

        curr = entry.ifa_next;
    }

    unsafe { libc::freeifaddrs(ifaddrs) };

    (best_iface, best_ip, vpn_active, vpn_name)
}

fn get_wifi_ssid() -> String {
    // macOS: use airport command — no easy non-ObjC alternative for SSID
    let output = Command::new(
        "/System/Library/PrivateFrameworks/Apple80211.framework/Versions/Current/Resources/airport",
    )
    .args(["-I"])
    .output()
    .ok();

    output
        .and_then(|o| {
            if !o.status.success() {
                return None;
            }
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout
                .lines()
                .find(|l| l.trim().starts_with("SSID:"))
                .map(|l| {
                    l.trim()
                        .strip_prefix("SSID:")
                        .unwrap_or("")
                        .trim()
                        .to_string()
                })
        })
        .unwrap_or_default()
}
