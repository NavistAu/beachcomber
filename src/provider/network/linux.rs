use std::process::Command;

pub fn is_vpn_interface(name: &str) -> bool {
    name.starts_with("tun") || name.starts_with("tap") || name.starts_with("wg")
}

pub fn is_preferred_interface(name: &str) -> bool {
    if let Some(default_iface) = get_default_route_interface() {
        return name == default_iface;
    }
    is_preferred_interface_by_prefix(name)
}

pub fn is_preferred_interface_by_prefix(name: &str) -> bool {
    name.starts_with("eth") || name.starts_with("ens") || name.starts_with("enp")
}

fn get_default_route_interface() -> Option<String> {
    let output = Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_default_route(&stdout)
}

pub fn parse_default_route(output: &str) -> Option<String> {
    let first_line = output.lines().next()?;
    let mut tokens = first_line.split_whitespace();
    while let Some(token) = tokens.next() {
        if token == "dev" {
            return tokens.next().map(|s| s.to_string());
        }
    }
    None
}

pub fn get_wifi_ssid() -> String {
    if let Some(ssid) = get_ssid_nmcli() {
        return ssid;
    }
    get_ssid_iw().unwrap_or_default()
}

fn get_ssid_nmcli() -> Option<String> {
    let output = Command::new("nmcli")
        .args(["-t", "-f", "active,ssid", "dev", "wifi"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_nmcli_output(&stdout)
}

pub fn parse_nmcli_output(output: &str) -> Option<String> {
    for line in output.lines() {
        if let Some(ssid) = line.strip_prefix("yes:") {
            let ssid = ssid.trim();
            if !ssid.is_empty() {
                return Some(ssid.to_string());
            }
        }
    }
    None
}

fn get_ssid_iw() -> Option<String> {
    let output = Command::new("iw").args(["dev"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let iface = parse_iw_dev_interface(&stdout)?;

    let info = Command::new("iw")
        .args(["dev", &iface, "info"])
        .output()
        .ok()?;
    if !info.status.success() {
        return None;
    }
    let info_str = String::from_utf8_lossy(&info.stdout);
    parse_iw_info_ssid(&info_str)
}

pub fn parse_iw_dev_interface(output: &str) -> Option<String> {
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(iface) = trimmed.strip_prefix("Interface ") {
            return Some(iface.trim().to_string());
        }
    }
    None
}

pub fn parse_iw_info_ssid(output: &str) -> Option<String> {
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(ssid) = trimmed.strip_prefix("ssid ") {
            let ssid = ssid.trim();
            if !ssid.is_empty() {
                return Some(ssid.to_string());
            }
        }
    }
    None
}
