//! `sys`: the plugin compiled into comb, for what only the OS can say.
//!
//! Each symbol is a plugin symbol like any library's, `extern "C"
//! fn(*const c_char) -> *const c_char` (the contract at
//! [`crate::engine::PluginSet`]), reached by `plugin('sys', '<symbol>')`
//! through [`super::StaticLibrary`] and the one call path every plugin has
//! ([`crate::boundaries::library::call_symbol`]). Only how the function is
//! found differs: this table, not `dlsym`.
//!
//! | symbol | answers |
//! |---|---|
//! | `hostname` | `{name, short}` (`gethostname`; `short` is its first label) |
//! | `loadavg` | `{one, five, fifteen}` (`getloadavg`) |
//! | `uname` | `{sysname, release, version, machine}` (`uname`) |
//! | `uptime` | `{seconds}` (macOS `sysctl KERN_BOOTTIME`, Linux `/proc/uptime`) |
//! | `user` | `{name, uid}` (`getuid`, `getpwuid_r`; `name` is absent for a uid with no entry) |
//! | `network` | `{interface, ip, ipv6, vpn_active, vpn_name, ssid, online}` (`getifaddrs`; the SSID from `airport` on macOS, `nmcli` then `iw` on Linux) |
//! | `battery` | macOS `{percent, charging, status, time_remaining_secs}` (`pmset -g batt`); Linux `{percent, charging, status_raw, status, time_remaining_secs}` (sysfs); `{}` with no battery |
//!
//! A symbol takes no arguments: its input is `[]`. It answers an object and
//! leaves out a fact it has no value for, never `""`. `NULL` is failure: the
//! call it wraps failed, or it was given arguments.
//!
//! What each wraps is reentrant (`gethostname`, `getloadavg`, `uname`,
//! `getpwuid_r`, `getifaddrs`, `sysctl`, file reads, a child process), so two
//! threads may call at once; an answer's lifetime is [`crate::plugin`]'s.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod parse;

#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
use macos as os;

use crate::plugin::Facts;
use serde_json::Value as Json;
use std::ffi::{CStr, c_char};
use std::net::{Ipv4Addr, Ipv6Addr};

use os::{is_vpn_interface, preferred_interface, uptime_secs, wifi_ssid};

crate::plugin::symbols! {
    no_args;
    battery => os::battery_facts,
    hostname => hostname_facts,
    loadavg => loadavg_facts,
    network => network_facts,
    uname => uname_facts,
    uptime => uptime_facts,
    user => user_facts,
}

/// `facts()`: `None` when it was given arguments.
fn no_args(facts: fn() -> Option<Facts>, args: Vec<Json>) -> Option<Facts> {
    if args.is_empty() { facts() } else { None }
}

/// `pairs` whose text is not empty.
fn texts<'a>(pairs: impl IntoIterator<Item = (&'a str, String)>) -> Facts {
    pairs
        .into_iter()
        .filter(|(_, text)| !text.is_empty())
        .map(|(key, text)| (key.to_string(), Json::from(text)))
        .collect()
}

/// `gethostname(3)`: `None` when it fails or the name is empty.
fn gethostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is writable for its length.
    let ret = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if ret != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned()).filter(|name| !name.is_empty())
}

fn hostname_facts() -> Option<Facts> {
    let name = gethostname()?;
    let short = name.split('.').next().unwrap_or(&name).to_string();
    Some(texts([("name", name), ("short", short)]))
}

/// `getloadavg(3)`: the 1, 5 and 15 minute averages.
fn loadavg() -> Option<[f64; 3]> {
    let mut avg = [0f64; 3];
    // SAFETY: `avg` holds the three samples asked for.
    (unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) } == 3).then_some(avg)
}

fn loadavg_facts() -> Option<Facts> {
    let [one, five, fifteen] = loadavg()?;
    Some(Facts::from_iter([
        ("one".to_string(), Json::from(one)),
        ("five".to_string(), Json::from(five)),
        ("fifteen".to_string(), Json::from(fifteen)),
    ]))
}

/// What `uname(2)` says of the system.
struct Uname {
    sysname: String,
    release: String,
    version: String,
    machine: String,
}

/// `uname(2)`.
fn uname() -> Option<Uname> {
    fn text(field: &[c_char]) -> String {
        let bytes: Vec<u8> = field
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
    // SAFETY: an all-zero `utsname` is a valid one for `uname` to fill.
    let mut buf: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: `buf` is a properly sized out-parameter.
    if unsafe { libc::uname(&mut buf) } != 0 {
        return None;
    }
    Some(Uname {
        sysname: text(&buf.sysname),
        release: text(&buf.release),
        version: text(&buf.version),
        machine: text(&buf.machine),
    })
}

fn uname_facts() -> Option<Facts> {
    let u = uname()?;
    Some(texts([
        ("sysname", u.sysname),
        ("release", u.release),
        ("version", u.version),
        ("machine", u.machine),
    ]))
}

fn uptime_facts() -> Option<Facts> {
    let seconds = uptime_secs().ok()?;
    Some(Facts::from_iter([(
        "seconds".to_string(),
        Json::from(seconds),
    )]))
}

/// The login name of `uid` (`getpwuid_r(3)`): `None` for a uid with no entry.
fn username(uid: libc::uid_t) -> Option<String> {
    // The buffer is `c_char`, signed on some targets and unsigned on others;
    // it grows on ERANGE. A zeroed `passwd` is portable across the platforms'
    // differing layouts, and `getpwuid_r` fills what it uses.
    let mut buf = vec![0 as c_char; 1024];
    // SAFETY: an all-zero `passwd` is a valid out-parameter.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    loop {
        // SAFETY: every pointer is to storage of the size passed with it.
        let ret =
            unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut found) };
        if ret == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        break;
    }
    if found.is_null() || pwd.pw_name.is_null() {
        return None;
    }
    // SAFETY: `getpwuid_r` succeeded: `pw_name` points into `buf`, NUL-terminated.
    let name = unsafe { CStr::from_ptr(pwd.pw_name) }
        .to_string_lossy()
        .into_owned();
    Some(name).filter(|n| !n.is_empty())
}

fn user_facts() -> Option<Facts> {
    // SAFETY: `getuid` cannot fail.
    let uid = unsafe { libc::getuid() };
    Some(user_of(uid))
}

/// `{name, uid}` of `uid`, `name` absent when it has no entry.
fn user_of(uid: libc::uid_t) -> Facts {
    let mut facts = Facts::from_iter([("uid".to_string(), Json::from(uid))]);
    if let Some(name) = username(uid) {
        facts.insert("name".into(), Json::from(name));
    }
    facts
}

/// What one interface scan found. An empty string is "none found".
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Scan {
    iface: String,
    ip: String,
    ipv6: String,
    vpn_active: bool,
    vpn_name: String,
}

/// `getifaddrs(3)`: of the interfaces that are up and not loopback, the first
/// VPN by name, the IPv4 address of the first with a routable one or of the
/// one `preferred` names, and the first routable IPv6 address.
fn scan_interfaces(preferred: &dyn Fn(&str) -> bool) -> Scan {
    let mut ifaddrs: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: an out-parameter `getifaddrs` fills, freed below.
    if unsafe { libc::getifaddrs(&mut ifaddrs) } != 0 {
        return Scan::default();
    }
    let mut scan = Scan::default();
    let mut curr = ifaddrs;
    while !curr.is_null() {
        // SAFETY: a node of the list `getifaddrs` returned, not yet freed.
        let entry = unsafe { &*curr };
        curr = entry.ifa_next;
        // SAFETY: each node's name is a NUL-terminated string.
        let name = unsafe { CStr::from_ptr(entry.ifa_name) }
            .to_string_lossy()
            .into_owned();
        let up = entry.ifa_flags & (libc::IFF_UP as u32) != 0;
        let loopback = entry.ifa_flags & (libc::IFF_LOOPBACK as u32) != 0;
        if !up || loopback {
            continue;
        }
        if is_vpn_interface(&name) && !scan.vpn_active {
            scan.vpn_active = true;
            scan.vpn_name = name.clone();
        }
        if entry.ifa_addr.is_null() {
            continue;
        }
        // SAFETY: a non-null `ifa_addr` is a `sockaddr` of its family.
        let family = i32::from(unsafe { (*entry.ifa_addr).sa_family });
        if family == libc::AF_INET {
            // SAFETY: an AF_INET address is a `sockaddr_in`.
            let addr = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in>() };
            let ip = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
            if !ip.is_loopback()
                && !ip.is_link_local()
                && (scan.iface.is_empty() || preferred(&name))
            {
                scan.iface = name;
                scan.ip = ip.to_string();
            }
        } else if family == libc::AF_INET6 && scan.ipv6.is_empty() {
            // SAFETY: an AF_INET6 address is a `sockaddr_in6`.
            let addr = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in6>() };
            let ipv6 = Ipv6Addr::from(addr.sin6_addr.s6_addr);
            // Not loopback and not link-local (fe80::/10).
            if !ipv6.is_loopback() && ipv6.segments()[0] & 0xffc0 != 0xfe80 {
                scan.ipv6 = ipv6.to_string();
            }
        }
    }
    // SAFETY: the list `getifaddrs` returned, freed once.
    unsafe { libc::freeifaddrs(ifaddrs) };
    scan
}

/// A scan and an SSID as facts. What the machine has no value for (an
/// interface, an address, a VPN, a Wi-Fi network) is absent; `online` (an
/// address of either family) and `vpn_active` are always there.
fn network_of(scan: Scan, ssid: Option<String>) -> Facts {
    let online = !scan.ip.is_empty() || !scan.ipv6.is_empty();
    let mut facts = texts([
        ("interface", scan.iface),
        ("ip", scan.ip),
        ("ipv6", scan.ipv6),
        ("vpn_name", scan.vpn_name),
        ("ssid", ssid.unwrap_or_default()),
    ]);
    facts.insert("online".into(), Json::from(online));
    facts.insert("vpn_active".into(), Json::from(scan.vpn_active));
    facts
}

fn network_facts() -> Option<Facts> {
    let preferred = preferred_interface();
    Some(network_of(scan_interfaces(&preferred), wifi_ssid()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundaries::library::MockLibraryLoader;
    use crate::engine::{PluginSet, daemon_engine_env};
    use libbeachcomber::engine::EngineCtx;
    use serde_json::json;
    use std::sync::Arc;

    /// `plugin('sys', symbol)` through the daemon's engine, as a Source's
    /// `data` line calls it, as JSON.
    fn call(symbol: &str) -> Json {
        let plugins = PluginSet::new(vec![], Arc::new(MockLibraryLoader::new()));
        let env = daemon_engine_env(Arc::new(EngineCtx::default()), Arc::new(plugins));
        let out = env
            .render_str(&format!("{{{{ plugin('sys', '{symbol}') | tojson }}}}"), ())
            .unwrap_or_else(|e| panic!("{symbol}: {e:#}"));
        serde_json::from_str(&out).unwrap()
    }

    fn keys(v: &Json) -> Vec<&str> {
        let mut keys: Vec<&str> = v
            .as_object()
            .unwrap_or_else(|| panic!("not an object: {v}"))
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn hostname_is_a_name_and_its_first_label() {
        let v = call("hostname");
        assert_eq!(keys(&v), ["name", "short"]);
        let name = v["name"].as_str().unwrap();
        assert!(!name.is_empty());
        assert_eq!(v["short"], json!(name.split('.').next().unwrap()));
    }

    #[test]
    fn loadavg_is_three_numbers() {
        let v = call("loadavg");
        assert_eq!(keys(&v), ["fifteen", "five", "one"]);
        for k in ["one", "five", "fifteen"] {
            assert!(v[k].as_f64().is_some_and(|x| x >= 0.0), "{k}: {v}");
        }
    }

    #[test]
    fn uname_names_this_system() {
        let v = call("uname");
        assert_eq!(keys(&v), ["machine", "release", "sysname", "version"]);
        let want = if cfg!(target_os = "macos") {
            "Darwin"
        } else {
            "Linux"
        };
        assert_eq!(v["sysname"], json!(want));
    }

    /// Needs an unsandboxed host (`sysctl`, `/proc`): CI filters it by name.
    #[test]
    fn uptime_provider_executes() {
        let v = call("uptime");
        assert_eq!(keys(&v), ["seconds"]);
        assert!(v["seconds"].as_i64().is_some_and(|s| s > 0), "{v}");
    }

    #[test]
    fn user_is_this_process() {
        let v = call("user");
        // SAFETY: `getuid` cannot fail.
        let uid = unsafe { libc::getuid() };
        assert_eq!(v["uid"], json!(uid));
        if let Some(name) = v.get("name") {
            assert!(name.as_str().is_some_and(|n| !n.is_empty()), "{v}");
        }
    }

    /// `getpwuid_r` with a buffer of each call's own: eight threads at once
    /// all see the one name.
    #[test]
    fn username_concurrent_no_panic() {
        // SAFETY: `getuid` cannot fail.
        let uid = unsafe { libc::getuid() };
        let names: Vec<Option<String>> = (0..8)
            .map(|_| std::thread::spawn(move || username(uid)))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();
        assert!(names.iter().all(|n| *n == names[0]), "{names:?}");
    }

    /// A uid no entry names has no `name`: never its number in its place.
    #[test]
    fn a_user_with_no_entry_has_no_name() {
        let unassigned = (4_000_000_000..4_000_001_000)
            .find(|&uid| username(uid).is_none())
            .expect("an unassigned uid");
        assert_eq!(
            Json::Object(user_of(unassigned)),
            json!({"uid": unassigned})
        );
    }

    #[test]
    fn network_always_says_online_and_vpn_active() {
        let v = call("network");
        assert!(
            v["online"].is_boolean() && v["vpn_active"].is_boolean(),
            "{v}"
        );
        if let Some(ip) = v.get("ip") {
            assert!(ip.as_str().unwrap().parse::<Ipv4Addr>().is_ok(), "{v}");
        }
        for k in ["interface", "ip", "ipv6", "vpn_name", "ssid"] {
            assert_ne!(v.get(k), Some(&json!("")), "{k} is never empty: {v}");
        }
    }

    /// What the machine has no value for is absent, never `""`; the two
    /// flags are always values.
    #[test]
    fn network_fields_it_has_no_value_for_are_absent() {
        assert_eq!(
            Json::Object(network_of(Scan::default(), None)),
            json!({"online": false, "vpn_active": false})
        );
        let wired = Scan {
            iface: "en0".into(),
            ip: "10.0.0.2".into(),
            ..Scan::default()
        };
        assert_eq!(
            Json::Object(network_of(wired, Some(String::new()))),
            json!({"interface": "en0", "ip": "10.0.0.2", "online": true, "vpn_active": false})
        );
    }

    /// A machine without a battery answers `{}`; one with answers a percent
    /// and whether it charges.
    #[test]
    fn battery_is_empty_or_a_percent() {
        let v = call("battery");
        let facts = v.as_object().unwrap();
        if facts.is_empty() {
            return;
        }
        assert!(
            v["percent"]
                .as_i64()
                .is_some_and(|p| (0..=100).contains(&p)),
            "{v}"
        );
        assert!(v["charging"].is_boolean(), "{v}");
    }

    /// A symbol takes no arguments: given any, it fails as a library's
    /// symbol fails, with `NULL`.
    #[test]
    fn a_symbol_given_arguments_fails() {
        let plugins = PluginSet::new(vec![], Arc::new(MockLibraryLoader::new()));
        let env = daemon_engine_env(Arc::new(EngineCtx::default()), Arc::new(plugins));
        let err = env
            .render_str("{{ plugin('sys', 'hostname', 1) }}", ())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("plugin(): hostname in sys: returned NULL"),
            "{err}"
        );
    }

    #[test]
    fn the_table_is_sorted_and_names_each_symbol_once() {
        let names: Vec<&str> = SYMBOLS.iter().map(|(n, _)| *n).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted);
        assert_eq!(
            names,
            [
                "battery", "hostname", "loadavg", "network", "uname", "uptime", "user"
            ]
        );
    }

    /// An answer stays valid until the next call of the same symbol on the
    /// same thread: a call of another symbol between does not move it.
    #[test]
    fn an_answer_outlives_a_call_of_another_symbol() {
        let find = |name: &str| SYMBOLS.iter().find(|(n, _)| *n == name).unwrap().1;
        let input = std::ffi::CString::new("[]").unwrap();
        // SAFETY: the contract's input, a NUL-terminated JSON array.
        let host = unsafe { find("hostname")(input.as_ptr()) };
        let before = unsafe { CStr::from_ptr(host) }.to_owned();
        // SAFETY: as above.
        let _ = unsafe { find("loadavg")(input.as_ptr()) };
        assert_eq!(unsafe { CStr::from_ptr(host) }, before.as_c_str());
    }
}
