//! `defaults/ssh.toml`, `gpg.toml` and `kerberos.toml`: the shell's agents and
//! tickets. Each tool is a stand-in that answers by the variable the provider
//! is cached on (`SSH_AUTH_SOCK`, `GNUPGHOME`, `KRB5CCNAME`), so one daemon
//! walks every exit path the real tool has, one value of the variable each.

use super::{Scenario, scenario};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// A scenario whose daemon and callers find the stand-ins in `bin` first and
/// nothing else on `PATH`.
struct Agents {
    s: Scenario,
    _bin: tempfile::TempDir,
}

/// The stand-ins `tools` (name, script body) on a `PATH` of their own, then
/// the daemon, whose `enabled.template` checks look there.
fn agents(tools: &[(&str, &str)]) -> Agents {
    let bin = tempfile::tempdir().unwrap();
    let dir = bin.path().canonicalize().unwrap();
    for (name, body) in tools {
        crate::common::daemon::tool(&dir, name, &format!("#!/bin/sh\n{LOGGED}\n{body}\n"));
    }
    // `sh -c` runs the commands, and is all of `PATH` that is not ours.
    std::os::unix::fs::symlink("/bin/sh", dir.join("sh")).unwrap();
    // SAFETY: one test per process, before the daemon starts.
    unsafe { std::env::set_var("PATH", &dir) };
    Agents {
        s: scenario(),
        _bin: bin,
    }
}

impl Agents {
    fn get(&self, key: &str, vars: &[(&str, &str)]) -> Option<Value> {
        let dir: PathBuf = self.s.project.clone();
        let log = self.log();
        let log = log.to_str().unwrap();
        let vars: Vec<(&str, &str)> = vars.iter().copied().chain([("RUN_LOG", log)]).collect();
        self.s.get_in_env(key, Path::new(&dir), &vars)
    }

    /// Where every stand-in logs each run.
    fn log(&self) -> PathBuf {
        self._bin.path().join("runs.log")
    }

    /// How many times the stand-ins have run.
    fn runs(&self) -> usize {
        std::fs::read_to_string(self.log()).map_or(0, |s| s.lines().count())
    }
}

/// Each stand-in logs its run first.
const LOGGED: &str = r#"echo "$0 $*" >> "$RUN_LOG""#;

const SSH_ADD: &str = r#"case "$SSH_AUTH_SOCK" in
/keys) printf '%s\n' '256 SHA256:aaa josh@laptop (ED25519)' '3072 SHA256:bbb work key (with parens) (RSA)'; exit 0 ;;
/empty) echo 'The agent has no identities.'; exit 1 ;;
/broken) exit 255 ;;
*) echo 'Could not open a connection to your authentication agent.' >&2; exit 2 ;;
esac"#;

#[test]
fn ssh_agent_with_keys() {
    let a = agents(&[("ssh-add", SSH_ADD)]);
    let v = [("SSH_AUTH_SOCK", "/keys")];
    assert_eq!(a.get("ssh.agent", &v), Some(json!(true)));
    assert_eq!(a.get("ssh.keys", &v), Some(json!(2)));
    assert_eq!(
        a.get("ssh.identities", &v),
        Some(json!(["josh@laptop", "work key (with parens)"]))
    );
    assert_eq!(a.get("ssh.forwarded", &v), Some(json!(false)));
}

#[test]
fn ssh_agent_with_no_keys_is_a_running_agent_holding_none() {
    let a = agents(&[("ssh-add", SSH_ADD)]);
    let v = [("SSH_AUTH_SOCK", "/empty")];
    assert_eq!(a.get("ssh.agent", &v), Some(json!(true)));
    assert_eq!(a.get("ssh.keys", &v), Some(json!(0)));
    assert_eq!(a.get("ssh.identities", &v), Some(json!([])));
}

/// No agent: no count of keys and no list of them, not `0` and `[]`.
#[test]
fn ssh_without_an_agent_has_no_keys_at_all() {
    let a = agents(&[("ssh-add", SSH_ADD)]);
    for v in [vec![], vec![("SSH_AUTH_SOCK", "/gone")]] {
        assert_eq!(a.get("ssh.agent", &v), Some(json!(false)));
        assert_eq!(a.get("ssh.keys", &v), None);
        assert_eq!(a.get("ssh.identities", &v), None);
        assert_eq!(a.get("ssh.forwarded", &v), Some(json!(false)));
        assert_eq!(
            a.get("ssh", &v),
            Some(json!({"agent": false, "forwarded": false}))
        );
    }
}

/// One `ssh-add -l` serves every field of a read, and the next read is its
/// memo's.
#[test]
fn ssh_runs_ssh_add_once_per_read() {
    let a = agents(&[("ssh-add", SSH_ADD)]);
    let v = [("SSH_AUTH_SOCK", "/keys")];
    assert_eq!(
        a.get("ssh", &v),
        Some(json!({
            "agent": true,
            "keys": 2,
            "identities": ["josh@laptop", "work key (with parens)"],
            "forwarded": false,
        }))
    );
    assert_eq!(a.runs(), 1);
    a.get("ssh", &v);
    a.get("ssh.keys", &v);
    assert_eq!(a.runs(), 1, "memoised for 30 seconds");
}

#[test]
fn ssh_forwarded_is_the_sshd_socket_path() {
    let a = agents(&[("ssh-add", SSH_ADD)]);
    let at = |p| a.get("ssh.forwarded", &[("SSH_AUTH_SOCK", p)]);
    assert_eq!(at("/tmp/ssh-XXkq3J/agent.4121"), Some(json!(true)));
    assert_eq!(at("/tmp/ssh-agent.sock"), Some(json!(false)));
    assert_eq!(
        at("/private/tmp/com.apple.launchd.x/Listeners"),
        Some(json!(false))
    );
}

#[test]
fn ssh_other_failure_is_an_error_not_a_guess() {
    let a = agents(&[("ssh-add", SSH_ADD)]);
    let v = [("SSH_AUTH_SOCK", "/broken")];
    assert_eq!(a.get("ssh.agent", &v), None);
    assert_eq!(a.get("ssh.keys", &v), None);
}

#[test]
fn ssh_is_off_where_ssh_add_is_not_installed() {
    let a = agents(&[]);
    assert_eq!(a.get("ssh.agent", &[("SSH_AUTH_SOCK", "/keys")]), None);
}

const GPG: &str = r#"case "$GNUPGHOME" in
/cached) printf '%s\n' \
  'S KEYINFO 5108D49D2666EF79925288CB381ECA2B59421924 D - - 1 P - - -' \
  'S KEYINFO 0AD00DFC345B1B47798A51957A386FAC31D2F8B7 D - - - P - - -' \
  'S KEYINFO 1111111111111111111111111111111111111111 T 0123 - 1 P - - -' OK ;;
/cold) printf '%s\n' 'S KEYINFO 5108D49D2666EF79925288CB381ECA2B59421924 D - - - P - - -' OK ;;
/nokeys) echo OK ;;
/exit1) echo 'gpg-connect-agent: no gpg-agent running in this session' >&2; exit 1 ;;
*) echo 'gpg-connect-agent: no gpg-agent running in this session' >&2 ;;
esac"#;

#[test]
fn gpg_counts_keys_and_cached_passphrases() {
    let a = agents(&[("gpg-connect-agent", GPG)]);
    let v = [("GNUPGHOME", "/cached")];
    assert_eq!(a.get("gpg.agent", &v), Some(json!(true)));
    assert_eq!(a.get("gpg.keys", &v), Some(json!(3)));
    assert_eq!(a.get("gpg.cached", &v), Some(json!(2)));
    let v = [("GNUPGHOME", "/cold")];
    assert_eq!(a.get("gpg.keys", &v), Some(json!(1)));
    assert_eq!(a.get("gpg.cached", &v), Some(json!(0)));
    let v = [("GNUPGHOME", "/nokeys")];
    assert_eq!(a.get("gpg.agent", &v), Some(json!(true)));
    assert_eq!(a.get("gpg.keys", &v), Some(json!(0)));
    assert_eq!(a.get("gpg.cached", &v), Some(json!(0)));
}

/// With no agent, whatever the exit status, the tool prints no `OK`.
#[test]
fn gpg_without_an_agent_holds_no_keys() {
    let a = agents(&[("gpg-connect-agent", GPG)]);
    for home in ["/exit0", "/exit1"] {
        let v = [("GNUPGHOME", home)];
        assert_eq!(a.get("gpg.agent", &v), Some(json!(false)), "{home}");
        assert_eq!(a.get("gpg.keys", &v), None, "{home}");
        assert_eq!(a.get("gpg.cached", &v), None, "{home}");
    }
}

/// One `gpg-connect-agent` serves every field of a read.
#[test]
fn gpg_runs_once_per_read() {
    let a = agents(&[("gpg-connect-agent", GPG)]);
    assert_eq!(
        a.get("gpg", &[("GNUPGHOME", "/cached")]),
        Some(json!({"agent": true, "keys": 3, "cached": 2}))
    );
    assert_eq!(a.runs(), 1);
}

#[test]
fn gpg_asks_without_starting_an_agent() {
    let a = agents(&[(
        "gpg-connect-agent",
        "[ \"$1\" = --no-autostart ] && echo OK",
    )]);
    assert_eq!(
        a.get("gpg.agent", &[("GNUPGHOME", "/x")]),
        Some(json!(true))
    );
}

const KLIST: &str = r#"case "$KRB5CCNAME" in
/mit) [ "$1" = -s ] && exit 0
  printf '%s\n' 'Ticket cache: FILE:/tmp/krb5cc_1000' 'Default principal: josh@EXAMPLE.COM' '' 'Valid starting     Expires            Service principal'
  exit 0 ;;
/heimdal) [ "$1" = -s ] && exit 0
  printf '%s\n' 'Credentials cache: API:ABC' '        Principal: josh@HEIMDAL.ORG' '' '  Issued                Expires               Principal'
  exit 0 ;;
/expired) [ "$1" = -s ] && exit 1
  printf '%s\n' 'Ticket cache: FILE:/tmp/krb5cc_1000' 'Default principal: josh@EXAMPLE.COM'
  exit 0 ;;
/broken) exit 255 ;;
*) echo 'klist: No credentials cache found (filename: /tmp/krb5cc_1000)' >&2; exit 1 ;;
esac"#;

#[test]
fn kerberos_valid_ticket_and_its_principal_in_mit_and_heimdal() {
    let a = agents(&[("klist", KLIST)]);
    for (cache, principal) in [
        ("/mit", "josh@EXAMPLE.COM"),
        ("/heimdal", "josh@HEIMDAL.ORG"),
    ] {
        let v = [("KRB5CCNAME", cache)];
        assert_eq!(a.get("kerberos.ticket", &v), Some(json!(true)), "{cache}");
        assert_eq!(
            a.get("kerberos.principal", &v),
            Some(json!(principal)),
            "{cache}"
        );
    }
}

/// One command serves both fields of a read: `klist -s`, then `klist`.
#[test]
fn kerberos_asks_once_per_read() {
    let a = agents(&[("klist", KLIST)]);
    assert_eq!(
        a.get("kerberos", &[("KRB5CCNAME", "/mit")]),
        Some(json!({"ticket": true, "principal": "josh@EXAMPLE.COM"}))
    );
    assert_eq!(a.runs(), 2, "klist -s and klist, once each");
}

#[test]
fn kerberos_expired_ticket_is_false_with_its_principal() {
    let a = agents(&[("klist", KLIST)]);
    let v = [("KRB5CCNAME", "/expired")];
    assert_eq!(a.get("kerberos.ticket", &v), Some(json!(false)));
    assert_eq!(
        a.get("kerberos.principal", &v),
        Some(json!("josh@EXAMPLE.COM"))
    );
}

#[test]
fn kerberos_no_cache_is_no_ticket_and_no_principal() {
    let a = agents(&[("klist", KLIST)]);
    for v in [vec![], vec![("KRB5CCNAME", "/none")]] {
        assert_eq!(a.get("kerberos.ticket", &v), Some(json!(false)));
        assert_eq!(a.get("kerberos.principal", &v), None);
    }
    assert_eq!(a.get("kerberos.ticket", &[("KRB5CCNAME", "/broken")]), None);
}
