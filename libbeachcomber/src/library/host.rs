//! The machine a template runs on: `_.os` and `_.net`, values under `_`
//! rather than calls. Both read natively (libc, `/proc`, a routing sysctl);
//! the one command, `scutil --dns` on macOS, is comb's, fixed, and not the
//! template's to choose.
//!
//! The parsers are pure functions over text or bytes, compiled on every
//! platform, so a recorded sample of another OS's file is tested everywhere.

use crate::engine::ctx;
use minijinja::Environment;
use minijinja::value::{Enumerator, Object, ObjectRepr, Value as MjValue};
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, LazyLock, OnceLock};

pub(super) fn add_os(env: &mut Environment<'_>) {
    env.add_global("os", MjValue::from_object(Os));
}

pub(super) fn add_net(env: &mut Environment<'_>) {
    env.add_global("net", MjValue::from_object(Net));
}

// ── _.os ─────────────────────────────────────────────────────────────────────

/// `_.os`: `{name, arch, distro, version}` in `uname`'s spellings, read once
/// per process on first use. A part not known (`distro` on macOS) is absent.
#[derive(Debug)]
struct Os;

static OS: LazyLock<BTreeMap<&'static str, String>> = LazyLock::new(os_info);

impl Object for Os {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &MjValue) -> Option<MjValue> {
        OS.get(key.as_str()?).map(|v| MjValue::from(v.as_str()))
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Values(OS.keys().map(|k| MjValue::from(*k)).collect())
    }
}

fn os_info() -> BTreeMap<&'static str, String> {
    let mut out = BTreeMap::new();
    // SAFETY: `uname` fills a zeroed out-parameter; read only on success.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut u) } == 0 {
        let field = |f: &[libc::c_char]| {
            let bytes: Vec<u8> = f
                .iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c as u8)
                .collect();
            String::from_utf8_lossy(&bytes).into_owned()
        };
        out.insert("name", field(&u.sysname));
        out.insert("arch", field(&u.machine));
    }
    #[cfg(target_os = "macos")]
    if let Ok(text) = std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist")
        && let Some(v) = plist_string(&text, "ProductVersion")
    {
        out.insert("version", v);
    }
    #[cfg(target_os = "linux")]
    if let Some(text) = ["/etc/os-release", "/usr/lib/os-release"]
        .iter()
        .find_map(|f| std::fs::read_to_string(f).ok())
    {
        let release = os_release(&text);
        if let Some(id) = release.get("ID") {
            out.insert("distro", id.clone());
        }
        if let Some(v) = release.get("VERSION_ID") {
            out.insert("version", v.clone());
        }
    }
    out
}

/// The `<string>` after `<key>key</key>` in an XML property list.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn plist_string(text: &str, key: &str) -> Option<String> {
    let after = &text[text.find(&format!("<key>{key}</key>"))?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")?;
    Some(after[start..start + end].trim().to_string())
}

/// `os-release(5)`: `KEY=value` lines, the value bare or quoted.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn os_release(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| {
            let v = v.trim();
            let unquoted = ['"', '\'']
                .iter()
                .find_map(|q| v.strip_prefix(*q)?.strip_suffix(*q))
                .unwrap_or(v);
            (k.trim().to_string(), unquoted.replace("\\\"", "\""))
        })
        .collect()
}

// ── _.net ────────────────────────────────────────────────────────────────────

/// What `_.net` has read in one render: each part on first use.
#[derive(Debug, Default)]
pub struct NetCache {
    parts: [OnceLock<MjValue>; 4],
}

const NET_PARTS: &[&str] = &["interfaces", "route", "route6", "dns"];

/// `_.net`: `interfaces`, `route`, `route6` and `dns`, each read when it is
/// first read in a render and kept for the rest of it.
#[derive(Debug)]
struct Net;

impl Object for Net {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &MjValue) -> Option<MjValue> {
        let i = NET_PARTS.iter().position(|p| Some(*p) == key.as_str())?;
        let ctx = ctx();
        let v = ctx.net.parts[i]
            .get_or_init(|| match i {
                0 => interfaces(),
                1 => route(false),
                2 => route(true),
                _ => dns(),
            })
            .clone();
        (!v.is_undefined()).then_some(v)
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Str(NET_PARTS)
    }
}

/// An IPv6 address as shown: a link-local one without the scope the BSD
/// kernel embeds in its second word.
fn v6(mut b: [u8; 16]) -> Ipv6Addr {
    if b[0] == 0xfe && b[1] & 0xc0 == 0x80 {
        b[2] = 0;
        b[3] = 0;
    }
    Ipv6Addr::from(b)
}

/// Each interface by name: `{up, addrs}`, its IPv4 and IPv6 addresses.
fn interfaces() -> MjValue {
    let mut out: BTreeMap<String, (bool, Vec<String>)> = BTreeMap::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `getifaddrs` allocates the list it points `head` at; it is
    // walked while live and freed once, below.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return MjValue::UNDEFINED;
    }
    let mut p = head;
    while !p.is_null() {
        // SAFETY: a node of the list `getifaddrs` returned, not yet freed.
        let ifa = unsafe { &*p };
        p = ifa.ifa_next;
        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .into_owned();
        let entry = out.entry(name).or_default();
        entry.0 |= ifa.ifa_flags & libc::IFF_UP as libc::c_uint != 0;
        if ifa.ifa_addr.is_null() {
            continue;
        }
        // SAFETY: `ifa_addr` is a sockaddr of the family it says.
        let addr = unsafe {
            match i32::from((*ifa.ifa_addr).sa_family) {
                libc::AF_INET => {
                    let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                    Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(
                        sin.sin_addr.s_addr,
                    ))))
                }
                libc::AF_INET6 => {
                    let sin6 = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                    Some(IpAddr::V6(v6(sin6.sin6_addr.s6_addr)))
                }
                _ => None,
            }
        };
        if let Some(addr) = addr {
            entry.1.push(addr.to_string());
        }
    }
    // SAFETY: the list `getifaddrs` returned, freed once.
    unsafe { libc::freeifaddrs(head) };
    MjValue::from_pairs(out.into_iter().map(|(name, (up, addrs))| {
        (
            name,
            MjValue::from_pairs([("up", MjValue::from(up)), ("addrs", MjValue::from(addrs))]),
        )
    }))
}

/// A default route: the interface it leaves by and its gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Route {
    pub interface: String,
    pub gateway: IpAddr,
}

fn route(v6: bool) -> MjValue {
    match default_route(v6) {
        Some(r) => MjValue::from_pairs([
            ("interface", MjValue::from(r.interface)),
            ("gateway", MjValue::from(r.gateway.to_string())),
        ]),
        None => MjValue::UNDEFINED,
    }
}

#[cfg(target_os = "linux")]
fn default_route(v6: bool) -> Option<Route> {
    if v6 {
        proc_ipv6_route(&std::fs::read_to_string("/proc/net/ipv6_route").ok()?)
    } else {
        proc_route(&std::fs::read_to_string("/proc/net/route").ok()?)
    }
}

/// `/proc/net/route`'s default route of least metric. Addresses are hex of
/// the address's bytes as a host-order word.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn proc_route(text: &str) -> Option<Route> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let hex = |i: usize| u32::from_str_radix(f.get(i)?, 16).ok();
            let (dest, gw, flags, metric, mask) = (hex(1)?, hex(2)?, hex(3)?, hex(6)?, hex(7)?);
            (dest == 0 && mask == 0 && flags & 0x3 == 0x3).then(|| {
                let route = Route {
                    interface: f[0].to_string(),
                    gateway: IpAddr::V4(Ipv4Addr::from(gw.to_ne_bytes())),
                };
                (metric, route)
            })
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, r)| r)
}

/// `/proc/net/ipv6_route`'s default route of least metric: `::/0` with a
/// gateway.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn proc_ipv6_route(text: &str) -> Option<Route> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 || f[0].bytes().any(|b| b != b'0') || f[1] != "00" {
                return None;
            }
            let flags = u32::from_str_radix(f[8], 16).ok()?;
            let metric = u32::from_str_radix(f[5], 16).ok()?;
            let gw = u128::from_str_radix(f[4], 16).ok()?;
            (flags & 0x3 == 0x3 && gw != 0).then(|| {
                let route = Route {
                    interface: f[9].to_string(),
                    gateway: IpAddr::V6(Ipv6Addr::from(gw)),
                };
                (metric, route)
            })
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, r)| r)
}

#[cfg(target_os = "macos")]
fn default_route(v6: bool) -> Option<Route> {
    let family = if v6 { libc::AF_INET6 } else { libc::AF_INET };
    let mut mib = [
        libc::CTL_NET,
        libc::PF_ROUTE,
        0,
        family,
        libc::NET_RT_FLAGS,
        libc::RTF_GATEWAY,
    ];
    let mut len: libc::size_t = 0;
    // SAFETY: the sizing call, then the fill into a buffer of that size (a
    // route added between them is a short read: ENOMEM, and no route).
    let sysctl = |buf: *mut libc::c_void, len: &mut libc::size_t, mib: &mut [libc::c_int; 6]| unsafe {
        libc::sysctl(mib.as_mut_ptr(), 6, buf, len, std::ptr::null_mut(), 0)
    };
    if sysctl(std::ptr::null_mut(), &mut len, &mut mib) != 0 || len == 0 {
        return None;
    }
    len += 1024;
    let mut buf = vec![0u8; len];
    if sysctl(buf.as_mut_ptr().cast(), &mut len, &mut mib) != 0 {
        return None;
    }
    buf.truncate(len);
    let routes = rt_defaults(&buf);
    let primary = routes
        .iter()
        .find(|r| r.flags & libc::RTF_IFSCOPE == 0)
        .or(routes.first())?;
    let mut name = [0 as libc::c_char; libc::IF_NAMESIZE];
    // SAFETY: `name` is IF_NAMESIZE bytes, as `if_indextoname` requires.
    let named = unsafe { libc::if_indextoname(u32::from(primary.index), name.as_mut_ptr()) };
    if named.is_null() {
        return None;
    }
    Some(Route {
        interface: unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
        gateway: primary.gateway,
    })
}

/// A default route of a BSD routing-table dump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RtDefault {
    pub index: u16,
    pub flags: i32,
    pub gateway: IpAddr,
}

/// `sizeof(struct rt_msghdr)` on macOS (arm64 and x86_64 alike).
const RT_MSGHDR: usize = 92;
/// macOS's address families: a gateway that is a link (`AF_LINK`, 18) is no
/// address and is passed over.
const BSD_AF_INET: u8 = 2;
const BSD_AF_INET6: u8 = 30;

/// The default routes (destination `0.0.0.0`/`::`, an empty netmask) of a
/// `sysctl NET_RT_FLAGS` dump: `rt_msghdr` records, each followed by the
/// sockaddrs its `rtm_addrs` bits name (`RTA_DST`, `RTA_GATEWAY`,
/// `RTA_NETMASK`, ...), each padded to 4 bytes (`ROUNDUP`, a zero length
/// taking 4).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn rt_defaults(buf: &[u8]) -> Vec<RtDefault> {
    let u16_at = |b: &[u8], i: usize| u16::from_ne_bytes([b[i], b[i + 1]]);
    let i32_at = |b: &[u8], i: usize| i32::from_ne_bytes(b[i..i + 4].try_into().unwrap());
    let addr = |sa: &[u8]| -> Option<IpAddr> {
        match *sa.get(1)? {
            BSD_AF_INET => Some(IpAddr::V4(Ipv4Addr::from(
                <[u8; 4]>::try_from(sa.get(4..8)?).ok()?,
            ))),
            BSD_AF_INET6 => Some(IpAddr::V6(v6(sa.get(8..24)?.try_into().ok()?))),
            _ => None,
        }
    };
    let mut out = Vec::new();
    let mut at = 0;
    while at + RT_MSGHDR <= buf.len() {
        let len = usize::from(u16_at(buf, at));
        if len < RT_MSGHDR {
            break;
        }
        let msg = &buf[at..(at + len).min(buf.len())];
        at += len;
        let (index, flags, addrs) = (u16_at(msg, 4), i32_at(msg, 8), i32_at(msg, 12));
        let (mut dst, mut gateway, mut mask_empty) = (None, None, true);
        let mut off = RT_MSGHDR;
        for bit in 0..8 {
            if addrs & (1 << bit) == 0 {
                continue;
            }
            let Some(&sa_len) = msg.get(off) else { break };
            let sa_len = usize::from(sa_len);
            let sa = &msg[off..(off + sa_len).min(msg.len())];
            match bit {
                0 => dst = addr(sa),
                1 => gateway = addr(sa),
                2 => mask_empty = sa.get(4..).is_none_or(|m| m.iter().all(|&b| b == 0)),
                _ => {}
            }
            off += if sa_len == 0 {
                4
            } else {
                1 + ((sa_len - 1) | 3)
            };
        }
        if let (Some(dst), Some(gateway)) = (dst, gateway)
            && dst.is_unspecified()
            && mask_empty
        {
            out.push(RtDefault {
                index,
                flags,
                gateway,
            });
        }
    }
    out
}

/// A resolver: the servers it asks and the domains it serves or searches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Resolver {
    pub servers: Vec<String>,
    pub domains: Vec<String>,
}

fn dns() -> MjValue {
    MjValue::from_iter(resolvers().into_iter().map(|r| {
        MjValue::from_pairs([
            ("servers", MjValue::from(r.servers)),
            ("domains", MjValue::from(r.domains)),
        ])
    }))
}

#[cfg(target_os = "macos")]
fn resolvers() -> Vec<Resolver> {
    let req = crate::exec::ExecRequest {
        cmd: "/usr/sbin/scutil",
        args: &["--dns".to_string()],
        stdin: None,
        timeout: std::time::Duration::from_secs(2),
        env: &std::collections::HashMap::new(),
        cwd: None,
    };
    crate::exec::run(&req)
        .map(|text| scutil_dns(&text))
        .unwrap_or_default()
}

/// `scutil --dns`'s unscoped resolvers that name a server: each
/// `resolver #N` block's `nameserver[N]`, and its `domain` and
/// `search domain[N]`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn scutil_dns(text: &str) -> Vec<Resolver> {
    let mut out: Vec<Resolver> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with("DNS configuration (") {
            break;
        }
        if line.starts_with("resolver #") {
            out.push(Resolver::default());
            continue;
        }
        let (Some(r), Some((key, value))) = (out.last_mut(), line.split_once(':')) else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim().to_string());
        if key.starts_with("nameserver[") {
            r.servers.push(value);
        } else if (key == "domain" || key.starts_with("search domain["))
            && !r.domains.contains(&value)
        {
            r.domains.push(value);
        }
    }
    out.retain(|r| !r.servers.is_empty());
    out
}

#[cfg(target_os = "linux")]
fn resolvers() -> Vec<Resolver> {
    let ctx = ctx();
    let read = |path: &str| {
        ctx.touch(std::path::Path::new(path));
        std::fs::read_to_string(path).ok().map(|t| resolv_conf(&t))
    };
    let Some(mut r) = read("/etc/resolv.conf") else {
        return Vec::new();
    };
    // systemd-resolved's stub: the servers it forwards to are in its own file.
    if r.servers == ["127.0.0.53"]
        && let Some(upstream) = read("/run/systemd/resolve/resolv.conf")
    {
        r = upstream;
    }
    if r.servers.is_empty() {
        Vec::new()
    } else {
        vec![r]
    }
}

/// `resolv.conf(5)`: `nameserver`, `domain` and `search` lines.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn resolv_conf(text: &str) -> Resolver {
    let mut r = Resolver::default();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("nameserver") => r.servers.extend(words.next().map(String::from)),
            Some("domain" | "search") => {
                for d in words.take_while(|w| !w.starts_with(['#', ';'])) {
                    if !r.domains.iter().any(|x| x == d) {
                        r.domains.push(d.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::testing::{eval, eval_json};

    #[test]
    fn os_is_unames_spelling() {
        let name = eval("_.os.name").unwrap();
        #[cfg(target_os = "macos")]
        assert_eq!(name, "Darwin");
        #[cfg(target_os = "linux")]
        assert_eq!(name, "Linux");
        let arch = eval("_.os.arch").unwrap();
        assert!(
            ["arm64", "aarch64", "x86_64"].contains(&arch.as_str()),
            "{arch}"
        );
        #[cfg(target_os = "macos")]
        {
            assert_eq!(eval("_.os.distro is undefined").unwrap(), "True");
            assert!(eval("_.os.version").unwrap().contains('.'));
        }
        assert!(eval_json("_.os").contains("\"name\""));
    }

    #[test]
    fn a_system_version_plist_gives_its_product_version() {
        let plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>BuildID</key>
	<string>4E5C0B1C-1F3C-11F0-9A8E-6B8A1D3C2E4F</string>
	<key>ProductBuildVersion</key>
	<string>24G720</string>
	<key>ProductName</key>
	<string>macOS</string>
	<key>ProductVersion</key>
	<string>15.7.7</string>
</dict>
</plist>"#;
        assert_eq!(plist_string(plist, "ProductVersion").unwrap(), "15.7.7");
        assert_eq!(plist_string(plist, "Nope"), None);
    }

    #[test]
    fn os_release_reads_bare_and_quoted_values() {
        let text = "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\n# a comment\nID=ubuntu\nID_LIKE=debian\nPRETTY_NAME='Ubuntu 24.04.1 LTS'\n";
        let r = os_release(text);
        assert_eq!(r["ID"], "ubuntu");
        assert_eq!(r["VERSION_ID"], "24.04");
        assert_eq!(r["PRETTY_NAME"], "Ubuntu 24.04.1 LTS");
    }

    #[test]
    fn proc_net_route_gives_the_default_of_least_metric() {
        let text = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
eth0\t00000000\t0100000A\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
eth0\t0000000A\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";
        let r = proc_route(text).unwrap();
        assert_eq!(r.interface, "eth0");
        // The sample is a little-endian host's, as CI's and this one's are.
        if cfg!(target_endian = "little") {
            assert_eq!(r.gateway.to_string(), "10.0.0.1");
        }
        assert_eq!(proc_route("Iface\tDestination\n"), None);
    }

    #[test]
    fn proc_net_ipv6_route_gives_the_default_with_a_gateway() {
        let text = "\
00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000001 00000000 00450003 eth0
fd000000000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001 eth0
00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo
";
        let r = proc_ipv6_route(text).unwrap();
        assert_eq!(
            (r.interface.as_str(), r.gateway.to_string()),
            ("eth0", "fe80::1".into())
        );
    }

    /// One `rt_msghdr` record with `addrs` sockaddrs after it.
    fn rt_record(index: u16, flags: i32, addrs: &[&[u8]]) -> Vec<u8> {
        let mut msg = vec![0u8; RT_MSGHDR];
        msg[4..6].copy_from_slice(&index.to_ne_bytes());
        msg[8..12].copy_from_slice(&flags.to_ne_bytes());
        let bits: i32 = (0..addrs.len()).map(|b| 1 << b).sum();
        msg[12..16].copy_from_slice(&bits.to_ne_bytes());
        for sa in addrs {
            msg.extend_from_slice(sa);
            let pad = if sa.is_empty() {
                4
            } else {
                (4 - sa.len() % 4) % 4
            };
            msg.extend(std::iter::repeat_n(0, pad));
        }
        let len = msg.len() as u16;
        msg[0..2].copy_from_slice(&len.to_ne_bytes());
        msg
    }

    fn sin(ip: [u8; 4]) -> Vec<u8> {
        let mut sa = vec![16, BSD_AF_INET, 0, 0];
        sa.extend(ip);
        sa.extend([0; 8]);
        sa
    }

    #[test]
    fn a_routing_dump_gives_its_default_routes() {
        let mut buf = rt_record(
            7,
            0x1000000 | 0x3,
            &[&sin([0, 0, 0, 0]), &sin([10, 1, 0, 1]), &[]],
        );
        // A route to 10.2.0.0/16 by a gateway is no default.
        buf.extend(rt_record(
            4,
            0x3,
            &[
                &sin([10, 2, 0, 0]),
                &sin([10, 1, 0, 1]),
                &[8, 0, 0, 0, 255, 255, 0, 0],
            ],
        ));
        // A netmask cut short (length 5, padded to 8) of zero is the empty one.
        buf.extend(rt_record(
            4,
            0x3,
            &[&sin([0, 0, 0, 0]), &sin([192, 168, 1, 1]), &[5, 0, 0, 0, 0]],
        ));
        let mut sin6 = vec![28, BSD_AF_INET6, 0, 0, 0, 0, 0, 0];
        sin6.extend([0xfe, 0x80, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        sin6.extend([0; 4]);
        let mut dst6 = vec![28, BSD_AF_INET6];
        dst6.extend([0; 26]);
        buf.extend(rt_record(4, 0x3, &[&dst6, &sin6, &[]]));
        let got = rt_defaults(&buf);
        let shown: Vec<_> = got
            .iter()
            .map(|r| (r.index, r.gateway.to_string()))
            .collect();
        assert_eq!(
            shown,
            [
                (7, "10.1.0.1".into()),
                (4, "192.168.1.1".into()),
                (4, "fe80::1".into())
            ]
        );
        assert_eq!(got[0].flags & 0x1000000, 0x1000000);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn rt_msghdr_is_the_size_the_parser_reads() {
        assert_eq!(std::mem::size_of::<libc::rt_msghdr>(), RT_MSGHDR);
        assert_eq!(libc::AF_INET6, i32::from(BSD_AF_INET6));
    }

    #[test]
    fn scutil_dns_reads_its_unscoped_resolvers() {
        let text = "\nDNS configuration\n\nresolver #1\n  search domain[0] : c.d.example\n  search domain[1] : example\n  nameserver[0] : 10.0.0.2\n  nameserver[1] : fe80::1%en0\n  if_index : 17 (en14)\n  flags    : Request A records\n  reach    : 0x00020002 (Reachable,Directly Reachable Address)\n\nresolver #2\n  domain   : local\n  options  : mdns\n  timeout  : 5\n  order    : 300000\n\nresolver #3\n  domain   : corp.example\n  nameserver[0] : 10.8.0.1\n  port     : 53\n\nDNS configuration (for scoped queries)\n\nresolver #1\n  search domain[0] : lan\n  nameserver[0] : 192.168.1.1\n";
        let got = scutil_dns(text);
        assert_eq!(
            got,
            [
                Resolver {
                    servers: vec!["10.0.0.2".into(), "fe80::1%en0".into()],
                    domains: vec!["c.d.example".into(), "example".into()],
                },
                Resolver {
                    servers: vec!["10.8.0.1".into()],
                    domains: vec!["corp.example".into()],
                },
            ]
        );
    }

    #[test]
    fn resolv_conf_reads_servers_and_domains() {
        let text = "# Generated\nnameserver 1.1.1.1\nnameserver 2606:4700::1111\ndomain home.arpa\nsearch home.arpa corp.example # trailing\noptions edns0\n";
        let r = resolv_conf(text);
        assert_eq!(r.servers, ["1.1.1.1", "2606:4700::1111"]);
        assert_eq!(r.domains, ["home.arpa", "corp.example"]);
    }

    /// Each part read live: loopback is an interface, and `dns` is a list.
    #[test]
    fn net_reads_this_machine() {
        let lo = if cfg!(target_os = "macos") {
            "lo0"
        } else {
            "lo"
        };
        assert_eq!(
            eval(&format!("_.net.interfaces['{lo}'].up")).unwrap(),
            "True"
        );
        assert!(
            eval(&format!("_.net.interfaces['{lo}'].addrs"))
                .unwrap()
                .contains("127.0.0.1")
        );
        assert_eq!(eval("_.net.dns is sequence").unwrap(), "True");
        // A machine with no network has no default route: absent, not an error.
        assert!(eval("_.net.route.interface").is_ok());
        assert!(eval("_.net.nope is undefined").unwrap() == "True");
    }
}
