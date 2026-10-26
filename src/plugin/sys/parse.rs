//! What `sys` reads, as text, made into facts: the pure half of each symbol.
//!
//! Each parser is compiled on the platform that reads its text and in tests on
//! both, so the Linux parsers are tested on macOS too, and the reverse.

use super::Facts;
use serde_json::Value as Json;

/// `pmset -g batt`: `{percent, charging, status, time_remaining_secs}`, or no
/// facts when no line shows a battery (a machine without one). A missing
/// estimate (`(no estimate)`, or a full battery) is absent, never 0.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn pmset(output: &str) -> Facts {
    let mut percent: i64 = 0;
    let mut charging = false;
    let mut time_remaining_secs: Option<i64> = None;
    let mut status = "unknown";
    let mut found = false;

    for line in output.lines() {
        let line = line.trim();
        let Some(pct_pos) = line.find('%') else {
            continue;
        };
        found = true;
        let digits: String = line[..pct_pos]
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        percent = digits.parse().unwrap_or(0);
        charging = line.contains("charging")
            && !line.contains("discharging")
            && !line.contains("not charging");
        let discharging = line.contains("discharging");

        if line.contains("(no estimate)") {
            status = "calculating";
            time_remaining_secs = None;
        } else if line.contains("charged") {
            status = "charged";
            time_remaining_secs = None;
        } else if let Some(rem_pos) = line.find("remaining") {
            if let Some(time) = line[..rem_pos].trim().rsplit(';').next()
                && let Some((h, m)) = time.trim().split_once(':')
                && let (Ok(hours), Ok(mins)) = (h.trim().parse::<i64>(), m.trim().parse::<i64>())
            {
                time_remaining_secs = Some(hours * 3600 + mins * 60);
            }
            status = if charging { "charging" } else { "discharging" };
        } else if charging {
            status = "charging";
        } else if discharging {
            status = "discharging";
        }
    }

    let mut facts = Facts::new();
    if !found {
        return facts;
    }
    facts.insert("percent".into(), Json::from(percent));
    facts.insert("charging".into(), Json::from(charging));
    facts.insert("status".into(), Json::from(status));
    if let Some(secs) = time_remaining_secs {
        facts.insert("time_remaining_secs".into(), Json::from(secs));
    }
    facts
}

/// A battery's sysfs directory, read one attribute at a time by `read` (its
/// trimmed text, `None` when the file is not there):
/// `{percent, charging, status_raw, status, time_remaining_secs}`.
///
/// `percent` is `capacity`; `status_raw` is `status` as the kernel writes it
/// and `charging` whether it is `Charging`. The estimate is the driver's own
/// `time_to_empty_now` (discharging) or `time_to_full_now` (charging) when it
/// gives one, else worked out from the energy (`energy_*`, µWh, over
/// `power_now`, µW) or the charge (`charge_*`, µAh, over `current_now`, µA):
/// what is left over the rate, discharging; what is missing over the rate,
/// charging. A rate of zero, or a status that is neither, is no estimate.
/// `status` is `charged` (Full), `charging` or `discharging` with an estimate,
/// `calculating` without one, else `unknown`.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn sysfs_battery(read: impl Fn(&str) -> Option<String>) -> Facts {
    let number = |name: &str| read(name).and_then(|s| s.parse::<f64>().ok());
    let mut facts = Facts::new();
    if let Some(percent) = read("capacity").and_then(|s| s.parse::<i64>().ok()) {
        facts.insert("percent".into(), Json::from(percent));
    }
    let raw = read("status").filter(|s| !s.is_empty());
    let discharging = raw.as_deref() == Some("Discharging");
    let charging = raw.as_deref() == Some("Charging");
    facts.insert("charging".into(), Json::from(charging));

    let estimate = |given: &str, left: fn(f64, f64) -> f64| -> Option<i64> {
        if let Some(secs) = number(given).filter(|s| *s > 0.0) {
            return Some(secs as i64);
        }
        [("energy", "power_now"), ("charge", "current_now")]
            .into_iter()
            .find_map(|(kind, rate)| {
                let now = number(&format!("{kind}_now"))?;
                let full = number(&format!("{kind}_full"));
                let rate = number(rate).filter(|r| *r > 0.0)?;
                let amount = left(now, full.unwrap_or(now));
                (amount > 0.0).then(|| (amount / rate * 3600.0) as i64)
            })
    };
    let time_remaining_secs = if discharging {
        estimate("time_to_empty_now", |now, _| now)
    } else if charging {
        estimate("time_to_full_now", |now, full| full - now)
    } else {
        None
    };

    let status = match raw.as_deref() {
        Some("Full") => "charged",
        Some("Charging") if time_remaining_secs.is_some() => "charging",
        Some("Discharging") if time_remaining_secs.is_some() => "discharging",
        Some("Charging" | "Discharging") => "calculating",
        _ => "unknown",
    };
    if let Some(raw) = raw {
        facts.insert("status_raw".into(), Json::from(raw));
    }
    facts.insert("status".into(), Json::from(status));
    if let Some(secs) = time_remaining_secs {
        facts.insert("time_remaining_secs".into(), Json::from(secs));
    }
    facts
}

/// `/proc/net/route`: the interface of the default route (destination and
/// mask `00000000`, flagged up), the lowest metric winning; `None` when there
/// is none.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn default_route(table: &str) -> Option<String> {
    const RTF_UP: u32 = 0x1;
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let (iface, dest, flags, metric, mask) = (
                cols.first()?,
                cols.get(1)?,
                cols.get(3)?,
                cols.get(6)?,
                cols.get(7)?,
            );
            let up = u32::from_str_radix(flags, 16).ok()? & RTF_UP != 0;
            (up && *dest == "00000000" && *mask == "00000000")
                .then(|| (metric.parse::<u64>().unwrap_or(u64::MAX), iface.to_string()))
        })
        .min()
        .map(|(_, iface)| iface)
}

/// `nmcli -t -f active,ssid dev wifi`: the SSID of the line marked active.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn nmcli_ssid(output: &str) -> Option<String> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix("yes:"))
        .map(str::trim)
        .find(|ssid| !ssid.is_empty())
        .map(str::to_string)
}

/// `iw dev`: the first wireless interface's name.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn iw_interface(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("Interface "))
        .map(|iface| iface.trim().to_string())
}

/// `iw dev <iface> info`: the SSID it is associated with.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn iw_ssid(output: &str) -> Option<String> {
    output
        .lines()
        .filter_map(|line| line.trim().strip_prefix("ssid "))
        .map(str::trim)
        .find(|ssid| !ssid.is_empty())
        .map(str::to_string)
}

/// `airport -I`: the SSID it is associated with.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn airport_ssid(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("SSID:"))
        .map(str::trim)
        .filter(|ssid| !ssid.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn obj(facts: Facts) -> Json {
        Json::Object(facts)
    }

    #[test]
    fn pmset_charging_with_estimate() {
        let sample =
            " -InternalBattery-0 (id=1234567)\t85%; charging; 1:23 remaining present: true\n";
        assert_eq!(
            obj(pmset(sample)),
            json!({"percent": 85, "charging": true, "status": "charging",
                   "time_remaining_secs": 4980})
        );
    }

    #[test]
    fn pmset_discharging_with_time_remaining() {
        let sample = " -InternalBattery-0\t50%; discharging; 2:30 remaining present: true\n";
        assert_eq!(
            obj(pmset(sample)),
            json!({"percent": 50, "charging": false, "status": "discharging",
                   "time_remaining_secs": 9000})
        );
    }

    /// No estimate is no value: absent, never 0.
    #[test]
    fn pmset_discharging_no_estimate() {
        let sample = " -InternalBattery-0\t72%; discharging; (no estimate) present: true\n";
        assert_eq!(
            obj(pmset(sample)),
            json!({"percent": 72, "charging": false, "status": "calculating"})
        );
    }

    /// A full battery has no time remaining to report.
    #[test]
    fn pmset_charged_state() {
        let sample = " -InternalBattery-0\t100%; charged; 0:00 remaining present: true\n";
        assert_eq!(
            obj(pmset(sample)),
            json!({"percent": 100, "charging": false, "status": "charged"})
        );
    }

    #[test]
    fn pmset_with_no_battery_has_no_facts() {
        assert!(pmset("Now drawing from 'AC Power'\n").is_empty());
        assert!(pmset("no battery info here\n").is_empty());
    }

    fn sysfs(files: &[(&str, &str)]) -> Json {
        let files: HashMap<String, String> = files
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        obj(sysfs_battery(|name| files.get(name).cloned()))
    }

    #[test]
    fn sysfs_discharging_from_energy_over_power() {
        assert_eq!(
            sysfs(&[
                ("capacity", "57"),
                ("status", "Discharging"),
                ("energy_now", "30000000"),
                ("energy_full", "50000000"),
                ("power_now", "10000000"),
            ]),
            json!({"percent": 57, "charging": false, "status_raw": "Discharging",
                   "status": "discharging", "time_remaining_secs": 10800})
        );
    }

    #[test]
    fn sysfs_charging_from_what_is_missing() {
        assert_eq!(
            sysfs(&[
                ("capacity", "60"),
                ("status", "Charging"),
                ("charge_now", "3000000"),
                ("charge_full", "5000000"),
                ("current_now", "2000000"),
            ]),
            json!({"percent": 60, "charging": true, "status_raw": "Charging",
                   "status": "charging", "time_remaining_secs": 3600})
        );
    }

    #[test]
    fn sysfs_the_drivers_own_estimate_wins() {
        let got = sysfs(&[
            ("status", "Discharging"),
            ("time_to_empty_now", "1234"),
            ("energy_now", "1"),
            ("power_now", "1"),
        ]);
        assert_eq!(got["time_remaining_secs"], json!(1234));
    }

    /// No rate is no estimate, and so `calculating`; a full battery is
    /// `charged`; a status the kernel does not name is `unknown`.
    #[test]
    fn sysfs_without_an_estimate() {
        let idle = sysfs(&[
            ("capacity", "80"),
            ("status", "Discharging"),
            ("energy_now", "1"),
            ("power_now", "0"),
        ]);
        assert_eq!(idle["status"], json!("calculating"));
        assert!(idle.get("time_remaining_secs").is_none(), "{idle}");
        let full = sysfs(&[("capacity", "100"), ("status", "Full")]);
        assert_eq!(full["status"], json!("charged"));
        assert_eq!(full["charging"], json!(false));
        let odd = sysfs(&[("status", "Not charging")]);
        assert_eq!(odd["status"], json!("unknown"));
        assert!(odd.get("percent").is_none(), "{odd}");
    }

    #[test]
    fn default_route_is_the_lowest_metric_up() {
        let table = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                     wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
                     enp0s3\t00000000\t0102A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
                     enp0s3\t0002A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n\
                     eth9\t00000000\t0103A8C0\t0002\t0\t0\t1\t00000000\t0\t0\t0\n";
        assert_eq!(default_route(table), Some("enp0s3".to_string()));
    }

    #[test]
    fn default_route_none() {
        let header =
            "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n";
        assert_eq!(default_route(header), None);
        assert_eq!(default_route(""), None);
    }

    #[test]
    fn parse_nmcli_active_network() {
        assert_eq!(
            nmcli_ssid("yes:HomeWifi\nno:NeighborWifi\n"),
            Some("HomeWifi".to_string())
        );
    }

    #[test]
    fn parse_nmcli_no_active() {
        assert_eq!(nmcli_ssid("no:SomeNetwork\nno:OtherNetwork\n"), None);
        assert_eq!(nmcli_ssid(""), None);
    }

    #[test]
    fn parse_iw_dev_interface() {
        assert_eq!(
            iw_interface("phy#0\n\tInterface wlan0\n\t\ttype managed\n"),
            Some("wlan0".to_string())
        );
    }

    #[test]
    fn parse_iw_info_ssid() {
        assert_eq!(
            iw_ssid("\tssid MyNetwork\n\ttype managed\n"),
            Some("MyNetwork".to_string())
        );
    }

    #[test]
    fn parse_airport_ssid() {
        let out = "     agrCtlRSSI: -50\n           SSID: Cafe Net\n        channel: 36\n";
        assert_eq!(airport_ssid(out), Some("Cafe Net".to_string()));
        assert_eq!(airport_ssid("AirPort: Off\n"), None);
    }
}
