//! `sys` on macOS: the boot time, the battery, the SSID, and which interfaces
//! are preferred and which are a VPN.

use super::{Facts, parse};
use std::process::Command;

/// Seconds since boot: the kernel's boot time (`sysctl KERN_BOOTTIME`)
/// against the clock.
pub(crate) fn uptime_secs() -> Result<i64, String> {
    let mut boottime = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut size = std::mem::size_of::<libc::timeval>();
    let mut mib = [libc::CTL_KERN, libc::KERN_BOOTTIME];
    // SAFETY: `boottime` and `size` describe a writable `timeval`.
    let ret = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&mut boottime as *mut libc::timeval).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret != 0 {
        return Err("sysctl kern.boottime failed".into());
    }
    // SAFETY: `time` with a null out-parameter only returns the clock.
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    Ok(now - boottime.tv_sec)
}

/// `pmset -g batt`: the battery's facts, `{}` when the machine has none;
/// `None` when `pmset` cannot be run or fails.
pub(super) fn battery_facts() -> Option<Facts> {
    let output = Command::new("pmset").args(["-g", "batt"]).output().ok()?;
    output
        .status
        .success()
        .then(|| parse::pmset(&String::from_utf8_lossy(&output.stdout)))
}

/// The SSID of the Wi-Fi network, from `airport -I`. The tool is gone from
/// macOS 14.4 on, where there is none: a known gap.
pub(crate) fn wifi_ssid() -> Option<String> {
    let output = Command::new(
        "/System/Library/PrivateFrameworks/Apple80211.framework/Versions/Current/Resources/airport",
    )
    .arg("-I")
    .output()
    .ok()?;
    if !output.status.success() {
        return None;
    }
    parse::airport_ssid(&String::from_utf8_lossy(&output.stdout))
}

/// A VPN's interface: `utun*`.
pub(crate) fn is_vpn_interface(name: &str) -> bool {
    name.starts_with("utun")
}

/// Which interface's address a scan prefers: an `en*` one (`en0` Wi-Fi,
/// `en1` Thunderbolt or USB Ethernet, and so on).
pub(crate) fn preferred_interface() -> impl Fn(&str) -> bool {
    |name: &str| name.starts_with("en") && !is_vpn_interface(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn en1_is_a_preferred_interface_and_a_vpn_is_not() {
        let preferred = preferred_interface();
        assert!(preferred("en0") && preferred("en1"));
        assert!(!preferred("utun3"));
        assert!(is_vpn_interface("utun3") && !is_vpn_interface("en0"));
    }
}
