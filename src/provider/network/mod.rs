use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::net::{Ipv4Addr, Ipv6Addr};

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;

pub struct NetworkProvider;

impl Provider for NetworkProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "network".into(),
            sources: vec![interfaces_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(NetworkInterfaces)]
    }
}

fn interfaces_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "interfaces".into(),
        fields: vec![
            FieldSchema {
                name: "interface".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "ip".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "vpn_active".into(),
                field_type: FieldType::Bool,
            },
            FieldSchema {
                name: "vpn_name".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "ssid".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "online".into(),
                field_type: FieldType::Bool,
            },
            FieldSchema {
                name: "ipv6".into(),
                field_type: FieldType::String,
            },
        ],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 10 },
        keep_alive: KeepAlive::Polls(6),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
    }
}

struct NetworkInterfaces;

impl Source for NetworkInterfaces {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(interfaces_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let (iface, ip, ipv6, vpn_active, vpn_name) = scan_interfaces();
        let ssid = get_wifi_ssid();

        // LIMITATION: online heuristic is IP-presence based (no reachability check).
        // A future improvement could probe a known host; out of S6 scope.
        let online = !ip.is_empty() || !ipv6.is_empty();

        let mut result = SourceResult::new();
        result.insert("interface", Value::String(iface));
        result.insert("ip", Value::String(ip));
        result.insert("ipv6", Value::String(ipv6));
        result.insert("online", Value::Bool(online));
        result.insert("vpn_active", Value::Bool(vpn_active));
        result.insert("vpn_name", Value::String(vpn_name));
        result.insert("ssid", Value::String(ssid));
        result
    }
}

fn scan_interfaces() -> (String, String, String, bool, String) {
    let mut ifaddrs: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut ifaddrs) } != 0 {
        return (
            String::new(),
            String::new(),
            String::new(),
            false,
            String::new(),
        );
    }

    let mut best_iface = String::new();
    let mut best_ip = String::new();
    let mut best_ipv6 = String::new();
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
                } else if family == libc::AF_INET6 && best_ipv6.is_empty() {
                    let addr = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in6) };
                    let ipv6 = Ipv6Addr::from(addr.sin6_addr.s6_addr);
                    // Skip link-local (fe80::/10) addresses.
                    if !ipv6.is_loopback() && !is_link_local_ipv6(&ipv6) {
                        best_ipv6 = ipv6.to_string();
                    }
                }
            }
        }

        curr = entry.ifa_next;
    }

    unsafe { libc::freeifaddrs(ifaddrs) };

    (best_iface, best_ip, best_ipv6, vpn_active, vpn_name)
}

/// Check if an IPv6 address is link-local (fe80::/10).
fn is_link_local_ipv6(addr: &Ipv6Addr) -> bool {
    let segments = addr.segments();
    (segments[0] & 0xffc0) == 0xfe80
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
