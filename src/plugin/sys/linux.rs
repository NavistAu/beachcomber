//! `sys` on Linux: the boot time, the battery, the SSID, and which interfaces
//! are preferred and which are a VPN. Everything but the SSID is a file read.

use super::{Facts, parse};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Seconds since boot: the first field of `/proc/uptime`.
pub(super) fn uptime_secs() -> Result<i64, String> {
    let contents =
        std::fs::read_to_string("/proc/uptime").map_err(|_| "cannot read /proc/uptime")?;
    contents
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|secs| secs as i64)
        .ok_or_else(|| "cannot parse /proc/uptime".into())
}

/// The first power supply of type `Battery` under `/sys/class/power_supply`.
fn find_battery_dir() -> Option<PathBuf> {
    std::fs::read_dir("/sys/class/power_supply")
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|dir| {
            std::fs::read_to_string(dir.join("type")).is_ok_and(|kind| kind.trim() == "Battery")
        })
}

/// The battery's facts from sysfs ([`parse::sysfs_battery`]), `{}` when the
/// machine has none.
pub(super) fn battery_facts() -> Option<Facts> {
    let Some(dir) = find_battery_dir() else {
        return Some(Facts::new());
    };
    Some(parse::sysfs_battery(|name| attribute(&dir, name)))
}

/// One sysfs attribute's trimmed text.
fn attribute(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name))
        .ok()
        .map(|text| text.trim().to_string())
}

/// The SSID of the Wi-Fi network: `nmcli`'s active one, else `iw`'s.
pub(super) fn wifi_ssid() -> Option<String> {
    let run = |args: &[&str]| -> Option<String> {
        let (program, args) = args.split_first()?;
        let output = Command::new(program).args(args).output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    if let Some(ssid) = run(&["nmcli", "-t", "-f", "active,ssid", "dev", "wifi"])
        .and_then(|out| parse::nmcli_ssid(&out))
    {
        return Some(ssid);
    }
    let iface = run(&["iw", "dev"]).and_then(|out| parse::iw_interface(&out))?;
    run(&["iw", "dev", &iface, "info"]).and_then(|out| parse::iw_ssid(&out))
}

/// A VPN's interface: `tun*`, `tap*`, `wg*`, `tailscale*`.
pub(super) fn is_vpn_interface(name: &str) -> bool {
    ["tun", "tap", "wg", "tailscale"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Which interface's address a scan prefers ([`parse::preferred_by_route`]):
/// the default route's, read once from `/proc/net/route`; with no default
/// route, a wired one by its name.
pub(super) fn preferred_interface() -> impl Fn(&str) -> bool {
    parse::preferred_by_route(std::fs::read_to_string("/proc/net/route").ok().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vpn_detection() {
        for vpn in ["tun0", "tap0", "wg0", "tailscale0"] {
            assert!(is_vpn_interface(vpn), "{vpn}");
        }
        for not in ["eth0", "enp0s3", "wlan0"] {
            assert!(!is_vpn_interface(not), "{not}");
        }
    }
}
