use std::process::Command;

/// Whether `name` is the interface whose address the scan prefers: the one
/// `ip route show default` names, else a wired one by its name. Spawned per
/// candidate, as it has been; `sys` reads `/proc/net/route` instead.
pub fn is_preferred_interface(name: &str) -> bool {
    if let Some(default_iface) = get_default_route_interface() {
        return name == default_iface;
    }
    crate::plugin::sys::preferred_by_prefix(name)
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
