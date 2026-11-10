//! Checks: is a tool installed (`which`), a process running (`pgrep`),
//! something listening (`listening`). Each reads and never acts, so a
//! template that may not act can call it; `listening` to a host other than
//! this one is the network, and asks the render whether it may
//! ([`EngineCtx::network`]).

use super::fs::resolve_path;
use super::{UNDEFINED, is_absent, is_absent_text};
use crate::engine::{EngineCtx, ctx, op_error};
use minijinja::value::{Kwargs, Value as MjValue};
use minijinja::{Environment, Error};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(super) fn add_which(env: &mut Environment<'_>) {
    env.add_filter("which", |name: MjValue| which(&ctx(), &name));
}

pub(super) fn add_pgrep(env: &mut Environment<'_>) {
    env.add_filter("pgrep", |pattern: MjValue, kwargs: Kwargs| {
        pgrep(&pattern, kwargs)
    });
}

pub(super) fn add_listening(env: &mut Environment<'_>) {
    env.add_filter("listening", |target: MjValue| listening(&ctx(), &target));
}

/// Whether `path` is a regular file the user may execute.
fn executable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a NUL-terminated path.
    path.is_file() && unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0
}

/// `name | which`: the full path of the first `name` on the render's `PATH`
/// that the user may execute, or undefined. A name holding a `/` is that
/// path, against the render's directory. Each directory searched is noted
/// where the render keeps account, so a tool installed later is a change.
fn which(ctx: &EngineCtx, name: &MjValue) -> Result<MjValue, Error> {
    if is_absent_text(name) {
        return Ok(UNDEFINED);
    }
    let Some(name) = name.as_str() else {
        return Err(op_error("which(): the name must be a string"));
    };
    let found = |p: PathBuf| MjValue::from(p.to_string_lossy().into_owned());
    if name.contains('/') {
        let path = resolve_path(ctx, name).map_err(|e| op_error(format!("which(): {e}")))?;
        ctx.touch(&path);
        return Ok(if executable(&path) {
            found(path)
        } else {
            UNDEFINED
        });
    }
    let path = ctx.env.get("PATH").map(String::as_str).unwrap_or_default();
    // An empty entry, the current directory to a shell, is passed over.
    for dir in path.split(':').filter(|d| !d.is_empty()) {
        let Ok(dir) = resolve_path(ctx, dir) else {
            continue;
        };
        ctx.touch(&dir);
        let candidate = dir.join(name);
        if executable(&candidate) {
            return Ok(found(candidate));
        }
    }
    Ok(UNDEFINED)
}

/// `pattern | pgrep(regex=false, full=false)`: the pids of every process
/// whose name matches, ascending, or undefined when none does. `pattern` is
/// a glob on the whole name (no wildcard is the exact name); `regex=true`
/// searches with a regex instead; `full=true` matches the command line, its
/// argv joined by spaces, in place of the name.
fn pgrep(pattern: &MjValue, kwargs: Kwargs) -> Result<MjValue, Error> {
    let flag = |key: &str| -> Result<bool, Error> {
        kwargs
            .get::<Option<bool>>(key)
            .map(|v| v.unwrap_or(false))
            .map_err(|_| op_error(format!("pgrep(): {key}= must be true or false")))
    };
    let (regex, full) = (flag("regex")?, flag("full")?);
    kwargs.assert_all_used()?;
    if is_absent_text(pattern) {
        return Ok(UNDEFINED);
    }
    let Some(p) = pattern.as_str() else {
        return Err(op_error("pgrep(): the pattern must be a string"));
    };
    let matches: Box<dyn Fn(&str) -> bool> = if regex {
        let re = regex::Regex::new(p)
            .map_err(|e| op_error(format!("pgrep(): invalid pattern {p:?}: {}", last_line(&e))))?;
        Box::new(move |s| re.is_match(s))
    } else {
        let glob = glob::Pattern::new(p)
            .map_err(|e| op_error(format!("pgrep(): invalid pattern {p:?}: {}", e.msg)))?;
        Box::new(move |s| glob.matches(s))
    };
    let mut pids: Vec<u32> = crate::proc::list_all(full)
        .into_iter()
        .filter(|proc| match (full, &proc.argv) {
            (true, Some(argv)) => matches(&argv.join(" ")),
            // A command line that cannot be read (another user's, on macOS)
            // is matched by the name.
            _ => matches(&proc.name),
        })
        .map(|proc| proc.pid)
        .collect();
    pids.sort_unstable();
    Ok(if pids.is_empty() {
        UNDEFINED
    } else {
        MjValue::from(pids)
    })
}

fn last_line(e: &regex::Error) -> String {
    let why = e.to_string();
    why.lines().last().unwrap_or("").trim().to_string()
}

/// How long `listening` waits for a connection.
const LISTENING_TIMEOUT: Duration = Duration::from_millis(200);

/// What `listening` connects to.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    Unix(PathBuf),
    /// Loopback addresses: this machine, no network.
    Local(Vec<SocketAddr>),
    Host(String, u16),
}

/// `target | listening`: true when something accepts a connection to
/// `target`, else undefined. A path (it holds a `/`) is a Unix socket; a
/// number, or digits, a port on loopback (127.0.0.1, then ::1); `host:port`
/// or `[v6]:port` that host. The connection is closed unused, after at most
/// 200ms. A host other than loopback is the network, which the render must
/// allow ([`EngineCtx::network`]).
fn listening(ctx: &EngineCtx, target: &MjValue) -> Result<MjValue, Error> {
    if is_absent(target) {
        return Ok(UNDEFINED);
    }
    let target = parse_target(target).map_err(|e| op_error(format!("listening(): {e}")))?;
    let answered = match target {
        Target::Unix(path) => {
            let path = resolve_path(ctx, &path.to_string_lossy())
                .map_err(|e| op_error(format!("listening(): {e}")))?;
            std::os::unix::net::UnixStream::connect(path).is_ok()
        }
        Target::Local(addrs) => connects(addrs),
        Target::Host(host, port) => {
            if !ctx.network {
                return Err(op_error(
                    "listening(): a host other than this one is the network, which runs only in a Source's lines",
                ));
            }
            // A name that does not resolve is nothing answering.
            connects(
                (host.as_str(), port)
                    .to_socket_addrs()
                    .into_iter()
                    .flatten(),
            )
        }
    };
    Ok(if answered {
        MjValue::from(true)
    } else {
        UNDEFINED
    })
}

fn connects(addrs: impl IntoIterator<Item = SocketAddr>) -> bool {
    addrs
        .into_iter()
        .any(|a| TcpStream::connect_timeout(&a, LISTENING_TIMEOUT).is_ok())
}

fn parse_target(v: &MjValue) -> Result<Target, String> {
    if let Some(n) = v.as_i64().filter(|_| v.as_str().is_none()) {
        return port(&n.to_string()).map(loopback);
    }
    let Some(s) = v.as_str() else {
        return Err("takes a socket path, a port or host:port".into());
    };
    if s.contains('/') || s.starts_with('~') {
        return Ok(Target::Unix(PathBuf::from(s)));
    }
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        return port(s).map(loopback);
    }
    let (host, p) = if let Some(rest) = s.strip_prefix('[') {
        rest.split_once("]:")
            .ok_or_else(|| format!("{s:?}: an IPv6 address is written [::1]:5432"))?
    } else {
        let (host, p) = s
            .rsplit_once(':')
            .ok_or_else(|| format!("{s:?} is not a socket path, a port or host:port"))?;
        if host.contains(':') {
            return Err(format!("{s:?}: an IPv6 address is written [::1]:5432"));
        }
        (host, p)
    };
    if host.is_empty() {
        return Err(format!("{s:?} has no host"));
    }
    let port = port(p)?;
    Ok(match host.parse::<IpAddr>() {
        Ok(ip) if ip.is_loopback() => Target::Local(vec![SocketAddr::new(ip, port)]),
        _ if host == "localhost" => loopback(port),
        _ => Target::Host(host.to_string(), port),
    })
}

/// `port` on 127.0.0.1, then ::1.
fn loopback(port: u16) -> Target {
    Target::Local(vec![
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
    ])
}

fn port(p: &str) -> Result<u16, String> {
    p.parse::<u16>()
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(|| format!("port {p:?} is not 1-65535"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::testing::{ctx_with, eval, eval_in};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    fn tool(dir: &Path, name: &str, mode: u32) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn which_finds_the_first_executable_on_the_render_path() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        tool(a.path(), "t", 0o644);
        std::fs::create_dir(a.path().join("d")).unwrap();
        let want = tool(b.path(), "t", 0o755);
        tool(b.path(), "d", 0o755);
        let path = format!("{}::{}", a.path().display(), b.path().display());
        let ctx = || ctx_with(&[("PATH", &path)], Some(a.path()));
        assert_eq!(
            eval_in(ctx(), "'t' | which").unwrap(),
            want.to_str().unwrap()
        );
        // A directory is passed over, as is a file not executable.
        assert_eq!(
            eval_in(ctx(), "'d' | which").unwrap(),
            b.path().join("d").to_str().unwrap()
        );
        assert_eq!(
            eval_in(ctx(), "'nope' | which is undefined").unwrap(),
            "True"
        );
        // A name with a `/` is that path, against the render's directory.
        tool(a.path(), "x", 0o755);
        assert_eq!(
            eval_in(ctx(), "'./x' | which").unwrap(),
            a.path().join("./x").to_str().unwrap()
        );
        assert_eq!(
            eval_in(ctx(), "'./t' | which is undefined").unwrap(),
            "True"
        );
        assert_eq!(eval("none | which is undefined").unwrap(), "True");
    }

    /// Each directory searched is an input: a tool installed in one is a
    /// change to it.
    #[test]
    fn which_notes_each_directory_it_searched() {
        let (a, b, c) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        tool(b.path(), "t", 0o755);
        let path = format!(
            "{}:{}:{}",
            a.path().display(),
            b.path().display(),
            c.path().display()
        );
        let touched = Arc::new(crate::fs::Touched::default());
        let mut ctx = ctx_with(&[("PATH", &path)], None);
        Arc::get_mut(&mut ctx).unwrap().touched = Some(touched.clone());
        eval_in(ctx, "'t' | which").unwrap();
        let read: Vec<PathBuf> = touched.touches().into_iter().map(|t| t.path).collect();
        assert_eq!(read, [a.path(), b.path()], "searched up to the hit");
        let before = touched.touches()[0].stamp;
        tool(a.path(), "t", 0o755);
        assert_ne!(crate::fs::stamp(a.path()), before, "installing is a change");
    }

    #[test]
    fn pgrep_matches_a_name_by_glob_or_regex_and_a_command_line() {
        let me = std::process::id().to_string();
        let name = crate::proc::list_all(false)
            .into_iter()
            .find(|p| p.pid == std::process::id())
            .unwrap()
            .name;
        let has_me = |expr: &str| {
            let got = eval(&format!("{expr} | join(',')")).unwrap();
            got.split(',').any(|p| p == me)
        };
        assert!(has_me(&format!("'{name}' | pgrep")), "exact name {name}");
        assert!(has_me(&format!("'{}*' | pgrep", &name[..3])), "glob");
        assert!(
            !has_me(&format!("'{}' | pgrep", &name[..3])),
            "a glob is the whole name"
        );
        assert!(has_me(&format!(
            "'^{}' | pgrep(regex=true)",
            regex::escape(&name[..3])
        )));
        let argv0 = std::env::args().next().unwrap();
        assert!(has_me(&format!("'{argv0}*' | pgrep(full=true)")), "full");
        assert_eq!(
            eval("'comb-no-such-process-*' | pgrep is undefined").unwrap(),
            "True"
        );
    }

    #[test]
    fn pgrep_finds_a_child_until_it_is_killed() {
        let mut child = std::process::Command::new("sleep")
            .arg("30.4217")
            .spawn()
            .unwrap();
        let pid = child.id().to_string();
        let found = eval("'sleep 30.4217' | pgrep(full=true) | join(',')").unwrap();
        // SAFETY: the pid of a child not yet reaped.
        unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
        child.wait().unwrap();
        assert_eq!(found, pid);
        assert_eq!(
            eval("'sleep 30\\\\.4217' | pgrep(regex=true, full=true) is undefined").unwrap(),
            "True"
        );
    }

    #[test]
    fn pgrep_bad_arguments_are_errors() {
        let err = eval("'(' | pgrep(regex=true)").unwrap_err();
        assert!(err.contains("pgrep(): invalid pattern"), "{err}");
        assert!(
            eval("'[' | pgrep")
                .unwrap_err()
                .contains("pgrep(): invalid pattern")
        );
        assert!(
            eval("'x' | pgrep(full='y')")
                .unwrap_err()
                .contains("pgrep(): full=")
        );
        assert!(eval("'x' | pgrep(nope=true)").is_err());
    }

    #[test]
    fn listening_connects_to_a_unix_socket_and_a_loopback_port() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("s");
        let _unix = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = tcp.local_addr().unwrap().port();
        let ctx = || ctx_with(&[], Some(dir.path()));
        assert_eq!(
            eval_in(ctx(), &format!("'{}' | listening", sock.display())).unwrap(),
            "True"
        );
        assert_eq!(eval_in(ctx(), "'./s' | listening").unwrap(), "True");
        assert_eq!(
            eval_in(ctx(), "'./gone' | listening is undefined").unwrap(),
            "True"
        );
        for t in [
            port.to_string(),
            format!("'{port}'"),
            format!("'localhost:{port}'"),
            format!("'127.0.0.1:{port}'"),
        ] {
            assert_eq!(
                eval_in(ctx(), &format!("{t} | listening")).unwrap(),
                "True",
                "{t}"
            );
        }
        drop(tcp);
        // Closed now: nothing answers.
        assert_eq!(
            eval_in(ctx(), &format!("{port} | listening is undefined")).unwrap(),
            "True"
        );
    }

    #[test]
    fn listening_to_another_host_is_the_network() {
        let err = eval("'example.com:443' | listening").unwrap_err();
        assert!(
            err.contains("listening(): a host other than this one is the network, which runs only in a Source's lines"),
            "{err}"
        );
        assert!(
            eval("'[2001:db8::1]:22' | listening")
                .unwrap_err()
                .contains("the network")
        );
        // `[::1]` is this machine.
        assert!(eval("'[::1]:1' | listening").is_ok());
        // Allowed, an address nothing answers at is undefined.
        let mut ctx = ctx_with(&[], None);
        Arc::get_mut(&mut ctx).unwrap().network = true;
        assert_eq!(
            eval_in(ctx, "'[2001:db8::1]:22' | listening is undefined").unwrap(),
            "True"
        );
    }

    #[test]
    fn listening_names_what_it_cannot_parse() {
        for (bad, why) in [
            ("'nope'", "is not a socket path, a port or host:port"),
            ("'::1:5432'", "an IPv6 address is written [::1]:5432"),
            ("'[::1]5432'", "an IPv6 address is written [::1]:5432"),
            ("70000", "port \"70000\" is not 1-65535"),
            ("'h:0'", "port \"0\" is not 1-65535"),
            ("':80'", "has no host"),
            ("[1]", "takes a socket path, a port or host:port"),
        ] {
            let err = eval(&format!("{bad} | listening")).unwrap_err();
            assert!(
                err.contains("listening(): ") && err.contains(why),
                "{bad}: {err}"
            );
        }
    }
}
