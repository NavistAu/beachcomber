//! `defaults/network.toml`: the network, from `sys`.

use super::scenario;
use serde_json::json;
use std::net::{Ipv4Addr, Ipv6Addr};

#[test]
fn network_provider_executes() {
    let s = scenario();
    let net = s.get("network", &s.project).expect("network answers");
    assert!(net["online"].is_boolean(), "{net}");
    assert!(net["vpn_active"].is_boolean(), "{net}");
    if let Some(ip) = net.get("ip") {
        assert!(ip.as_str().unwrap().parse::<Ipv4Addr>().is_ok(), "{net}");
    }
    if let Some(ip) = net.get("ipv6") {
        assert!(ip.as_str().unwrap().parse::<Ipv6Addr>().is_ok(), "{net}");
    }
    for field in ["interface", "ip", "ipv6", "vpn_name", "ssid"] {
        assert_ne!(net.get(field), Some(&json!("")), "{field}: {net}");
    }
    let leaf = s
        .get("network:cache.online", &s.project)
        .expect("a cache leaf");
    assert_eq!(
        leaf["poll"],
        json!({"interval": "10s", "count": 6}),
        "{leaf}"
    );
}
