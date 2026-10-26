//! Handler for the `get` (`g`) subcommand.
//!
//! `comb get` is a client of [`libbeachcomber::resolve`]. It parses keys, a
//! path and a format, makes sure the daemon is running, and hands every key to
//! the library's one resolver with this process's environment and directory
//! and `exec()` enabled. Nothing here decides what a key is worth: a template
//! the daemon holds is rendered in the library, a cached value comes back as
//! it is, `env.*` is answered from the environment. This module only renders
//! what comes back, per output format, and then writes back the memos the
//! keys' cached templates rendered: the output is out first.

use crate::cli::exit::Outcome;
use crate::cli::output_format::{Format, template_locals};
use crate::config::Config;
use libbeachcomber::render::{Format as ValueFormat, render_value, render_values};
use libbeachcomber::resolve::{Slot, eval_with, resolve_traced, write_back};
use serde_json::Value;

/// Whether `s` is a directory rather than a key: it starts with `/`, `~` or
/// `.`. No key can — a key's head is a provider name, and a provider name
/// holds no `.` and starts with neither `/` nor `~` — so this is syntax, not
/// a guess. A key may hold a `/` anywhere after its head
/// (`mise.npm:@scope/pkg`).
pub fn is_path_like(s: &str) -> bool {
    s.starts_with(['/', '~', '.'])
}

/// Separate keys from an optional trailing path positional.
///
/// 1. With `--path`, every positional is a key.
/// 2. Otherwise, when more than one positional is given and the last is a
///    directory (per `is_path_like`), it is the path.
/// 3. Otherwise there is no path, and the caller reads the current directory.
pub fn split_keys_and_path(
    mut positionals: Vec<String>,
    explicit_path: Option<String>,
) -> (Vec<String>, Option<String>) {
    if explicit_path.is_some() {
        return (positionals, explicit_path);
    }
    if positionals.len() > 1
        && let Some(last) = positionals.last()
        && is_path_like(last)
    {
        let path = positionals.pop();
        return (positionals, path);
    }
    (positionals, None)
}

/// `path` is what `main.rs` already settled: `None` for `--global`, otherwise
/// `--path`, the trailing positional, or the current directory.
pub fn run_get(
    config: &Config,
    keys: &[String],
    path: Option<&str>,
    format: Format,
    force: bool,
    wait: libbeachcomber::Wait,
) -> Outcome {
    // `comb` always requires the daemon (design §2), a key whose answer needs
    // no request (`env.HOME`) included.
    let socket_path = match crate::cli::connect::connect(config) {
        Ok(socket) => socket,
        Err(outcome) => return outcome,
    };
    let mut session = match crate::client::Client::new(socket_path).connect() {
        Ok(s) => s,
        Err(e) => {
            crate::cli::report::daemon_unreachable(e);
            return Outcome::Error;
        }
    };

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let env = crate::cli::shell_env();
    let slot = match path {
        Some(p) => Slot::Path(p.to_string()),
        None => Slot::Global,
    };
    let inputs = crate::cli::inputs(&cwd, &env, slot, force, wait);

    // One slot per key, `None` where it produced no value, so a multi-key
    // document keeps its positions. What the keys came to is decided here,
    // before any format is applied: a miss is a miss in every format.
    let mut slots: Vec<(&str, Option<Value>)> = Vec::with_capacity(keys.len());
    let mut memos = Vec::new();
    let mut outcome = Outcome::Done;
    for key in keys {
        let value = match resolve_traced(key, &inputs, &mut session) {
            Ok(traced) => {
                memos.extend(traced.memos);
                if traced.value.is_none() {
                    outcome = outcome.max(Outcome::Miss);
                }
                traced.value
            }
            Err(e) => {
                crate::cli::report::resolve_error(key, &e);
                outcome = Outcome::Error;
                None
            }
        };
        slots.push((key, value));
    }

    let rendered = match &format {
        // A template renders as `comb eval` renders one, with the values
        // bound beside every name a template reads.
        Format::Template(template) => eval_with(
            template,
            "-f",
            &inputs,
            &mut session,
            &template_locals(&slots),
        )
        .map(|v| render_value("", v.as_ref(), ValueFormat::Text))
        .map_err(|e| e.to_string()),
        Format::Preset(preset) => {
            let format = preset
                .value_format()
                .expect("get's formats are value formats");
            Ok(match slots.as_slice() {
                // A key that failed has no answer to print: its error is it.
                [_] if outcome == Outcome::Error => String::new(),
                [(key, value)] => render_value(key, value.as_ref(), format),
                many => {
                    let slots: Vec<(&str, Option<&Value>)> =
                        many.iter().map(|(k, v)| (*k, v.as_ref())).collect();
                    render_values(&slots, format)
                }
            })
        }
    };
    match rendered {
        Ok(out) => {
            print!("{out}");
            // The caller has its answer before the memos go back.
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        Err(e) => {
            crate::cli::report::error_named(e);
            outcome = Outcome::Error;
        }
    }
    write_back(&memos, &mut session);
    outcome
}

#[cfg(test)]
mod path_disambiguation_tests {
    use super::{is_path_like, split_keys_and_path};

    // --- is_path_like ---

    #[test]
    fn dot_alone_is_path_like() {
        assert!(is_path_like("."));
    }

    #[test]
    fn dot_prefix_is_path_like() {
        assert!(is_path_like("./subdir"));
        assert!(is_path_like(".hidden"));
    }

    #[test]
    fn tilde_prefix_is_path_like() {
        assert!(is_path_like("~/repos"));
        assert!(is_path_like("~"));
    }

    #[test]
    fn absolute_path_is_path_like() {
        assert!(is_path_like("/home/me/repo"));
        assert!(is_path_like("/"));
    }

    #[test]
    fn a_key_holding_a_slash_is_not_path_like() {
        assert!(!is_path_like("mise.npm:@scope/pkg"));
        assert!(!is_path_like("mise['go:golang.org/x/tools/gopls']"));
        assert!(!is_path_like("some/dir"));
    }

    #[test]
    fn provider_key_is_not_path_like() {
        assert!(!is_path_like("git.branch"));
        assert!(!is_path_like("hostname.name"));
        assert!(!is_path_like("git"));
        assert!(!is_path_like("user"));
    }

    // --- split_keys_and_path ---

    fn keys(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn single_key_trailing_dot_is_path() {
        // `comb get git.branch .` → key=git.branch, path=.
        let (ks, path) = split_keys_and_path(keys(&["git.branch", "."]), None);
        assert_eq!(ks, vec!["git.branch"]);
        assert_eq!(path.as_deref(), Some("."));
    }

    #[test]
    fn single_key_trailing_absolute_path_is_path() {
        // `comb get git.branch /home/me/repo`
        let (ks, path) = split_keys_and_path(keys(&["git.branch", "/home/me/repo"]), None);
        assert_eq!(ks, vec!["git.branch"]);
        assert_eq!(path.as_deref(), Some("/home/me/repo"));
    }

    #[test]
    fn single_key_trailing_relative_dot_prefix_is_path() {
        // `comb get git.branch ./subdir`
        let (ks, path) = split_keys_and_path(keys(&["git.branch", "./subdir"]), None);
        assert_eq!(ks, vec!["git.branch"]);
        assert_eq!(path.as_deref(), Some("./subdir"));
    }

    #[test]
    fn two_keys_no_path_returns_no_path() {
        // `comb get git.branch git.sha` → no path extracted; caller defaults to CWD
        let (ks, path) = split_keys_and_path(keys(&["git.branch", "git.sha"]), None);
        assert_eq!(ks, vec!["git.branch", "git.sha"]);
        assert!(path.is_none());
    }

    #[test]
    fn two_keys_trailing_dot_pops_path() {
        // `comb get git.branch user.name .`
        let (ks, path) = split_keys_and_path(keys(&["git.branch", "user.name", "."]), None);
        assert_eq!(ks, vec!["git.branch", "user.name"]);
        assert_eq!(path.as_deref(), Some("."));
    }

    #[test]
    fn explicit_flag_overrides_positional_dot() {
        // `--path /other` is set; the positional `.` is NOT treated as path — flag wins.
        let (ks, path) =
            split_keys_and_path(keys(&["git.branch", "."]), Some("/other".to_string()));
        // With explicit flag, no positional popping occurs — `.` stays as a key.
        assert_eq!(ks, vec!["git.branch", "."]);
        assert_eq!(path.as_deref(), Some("/other"));
    }

    #[test]
    fn single_key_that_looks_like_key_returns_no_path() {
        // `comb get git.branch` → key only, no path extracted
        let (ks, path) = split_keys_and_path(keys(&["git.branch"]), None);
        assert_eq!(ks, vec!["git.branch"]);
        assert!(path.is_none());
    }

    #[test]
    fn provider_only_key_is_not_path_like() {
        // `comb get git` — bare provider name, no dot, no slash
        let (ks, path) = split_keys_and_path(keys(&["git"]), None);
        assert_eq!(ks, vec!["git"]);
        assert!(path.is_none());
    }

    #[test]
    fn a_lone_positional_is_a_key_whatever_it_looks_like() {
        // `comb get mise.npm:@scope/pkg` and even `comb get .` name a key.
        for only in ["mise.npm:@scope/pkg", "."] {
            let (ks, path) = split_keys_and_path(keys(&[only]), None);
            assert_eq!(ks, vec![only]);
            assert!(path.is_none());
        }
    }

    #[test]
    fn a_trailing_key_holding_a_slash_stays_a_key() {
        let (ks, path) = split_keys_and_path(keys(&["git.branch", "mise.npm:@scope/pkg"]), None);
        assert_eq!(ks, vec!["git.branch", "mise.npm:@scope/pkg"]);
        assert!(path.is_none());
    }

    #[test]
    fn tilde_path_is_popped() {
        let (ks, path) = split_keys_and_path(keys(&["git.branch", "~/myrepo"]), None);
        assert_eq!(ks, vec!["git.branch"]);
        assert_eq!(path.as_deref(), Some("~/myrepo"));
    }
}
