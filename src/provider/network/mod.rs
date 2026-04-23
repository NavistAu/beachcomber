use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::net::Ipv4Addr;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
mod macos;

pub struct NetworkProvider;

impl Provider for NetworkProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "network".to_string(),
            fields: vec![
                FieldSchema {
                    name: "interface".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "ip".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "vpn_active".to_string(),
                    field_type: FieldType::Bool,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "vpn_name".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "ssid".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "online".to_string(),
                    field_type: FieldType::Bool,
                    scope: FieldScope::Global,
                },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 10,
                floor_secs: 5,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
        let (iface, ip, vpn_active, vpn_name) = scan_interfaces();
        let ssid = get_wifi_ssid();

        let mut result = ProviderResult::new();
        result.insert("interface", Value::String(iface));
        result.insert("ip", Value::String(ip.clone()));
        result.insert("online", Value::Bool(!ip.is_empty()));
        result.insert("vpn_active", Value::Bool(vpn_active));
        result.insert("vpn_name", Value::String(vpn_name));
        result.insert("ssid", Value::String(ssid));
        vec![(None, result)]
    }
}

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
            if is_vpn_interface(&name) && !vpn_active {
                vpn_active = true;
                vpn_name = name.clone();
            }

            if !entry.ifa_addr.is_null() {
                let family = unsafe { (*entry.ifa_addr).sa_family } as i32;
                if family == libc::AF_INET {
                    let addr = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in) };
                    let ip = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
                    if !ip.is_loopback()
                        && !ip.is_link_local()
                        && (best_iface.is_empty() || is_preferred_interface(&name))
                    {
                        best_iface = name.clone();
                        best_ip = ip.to_string();
                    }
                }
            }
        }

        curr = entry.ifa_next;
    }

    unsafe { libc::freeifaddrs(ifaddrs) };

    (best_iface, best_ip, vpn_active, vpn_name)
}

#[cfg(target_os = "macos")]
fn is_vpn_interface(name: &str) -> bool {
    macos::is_vpn_interface(name)
}
#[cfg(target_os = "linux")]
fn is_vpn_interface(name: &str) -> bool {
    linux::is_vpn_interface(name)
}

#[cfg(target_os = "macos")]
fn is_preferred_interface(name: &str) -> bool {
    macos::is_preferred_interface(name)
}
#[cfg(target_os = "linux")]
fn is_preferred_interface(name: &str) -> bool {
    linux::is_preferred_interface(name)
}

#[cfg(target_os = "macos")]
fn get_wifi_ssid() -> String {
    macos::get_wifi_ssid()
}
#[cfg(target_os = "linux")]
fn get_wifi_ssid() -> String {
    linux::get_wifi_ssid()
}
