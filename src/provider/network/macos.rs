use std::process::Command;

pub fn is_vpn_interface(name: &str) -> bool {
    name.starts_with("utun")
}

pub fn is_preferred_interface(name: &str) -> bool {
    // Prefer en* interfaces (en0=WiFi, en1=Thunderbolt/USB Ethernet, etc.)
    // over other non-VPN interfaces. The correct fix (reading the default route
    // via SCNetworkConfiguration) is a method improvement owned by S4. This
    // removes the en0 hardcode which caused en1 wired connections to be ignored.
    name.starts_with("en") && !is_vpn_interface(name)
}

pub fn get_wifi_ssid() -> String {
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
