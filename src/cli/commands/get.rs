//! Handler for the `get` (`g`) subcommand.
//!
//! `comb get` is a client of [`libbeachcomber::resolve`]. It parses keys, a
//! path and a format, makes sure the daemon is running, and hands every key to
//! the library's one resolver with this process's environment and directory
//! and `exec()` enabled. Nothing here decides what a key is worth: a template
//! the daemon holds is rendered in the library, a cached value comes back as
//! it is, `env.*` is answered from the environment. This module only renders
//! what comes back, per output format.

use crate::cli::format::render_fmt_template_json;
use crate::cli::output_format::{OutputFormat, format_sv, value_to_string};
use crate::config::Config;
use libbeachcomber::render::render_data;
use libbeachcomber::resolve::{Inputs, ResolveError, Slot, resolve};
use serde_json::Value;
use std::collections::HashMap;
use std::process::ExitCode;

/// Returns true if `s` looks like a filesystem path rather than a provider key.
///
/// A positional is treated as a path when it:
/// - equals `.` literally
/// - starts with `.` (relative dot-prefix like `./subdir`)
/// - starts with `~` (home-relative)
/// - starts with `/` (absolute)
/// - contains `/` anywhere (e.g., `some/dir`)
pub fn is_path_like(s: &str) -> bool {
    s == "." || s.starts_with('.') || s.starts_with('~') || s.starts_with('/') || s.contains('/')
}

/// Separate keys from an optional trailing path positional.
///
/// Rules:
/// 1. If `--path` flag was already set, all positionals are keys.
/// 2. Otherwise, if the last positional is path-like (per `is_path_like`), pop it as the path.
/// 3. If still no path, return `None` — the caller should default to CWD.
pub fn split_keys_and_path(
    mut positionals: Vec<String>,
    explicit_path: Option<String>,
) -> (Vec<String>, Option<String>) {
    if explicit_path.is_some() {
        return (positionals, explicit_path);
    }
    if let Some(last) = positionals.last()
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
    format: OutputFormat,
    force: bool,
    wait: bool,
) -> ExitCode {
    let (socket_path, socket_source) = config.resolve_socket_path_with_source();
    let spawn_no_reap = matches!(socket_source, crate::config::SocketPathSource::EnvVar);

    // `comb` always requires the daemon (design §2), a key whose answer needs
    // no request (`env.HOME`) included.
    if let Err(e) = crate::daemon::ensure_daemon(&socket_path, spawn_no_reap) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }
    let mut session = match crate::client::Client::new(socket_path).connect() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::from(2);
        }
    };

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let env: HashMap<String, String> = std::env::vars().collect();
    let slot = match path {
        Some(p) => Slot::Path(p.to_string()),
        None => Slot::Global,
    };
    let inputs = Inputs {
        cwd: &cwd,
        env: &env,
        slot,
        force,
        wait,
        allow_exec: true,
    };

    // One slot per key, `None` where it produced no value, so a multi-key
    // document keeps its positions. Under `text`/`sh` a miss is the empty
    // string — nothing to print, not a failure — so a value embeds in a prompt
    // verbatim. Under every structured format it is an error: there is no
    // value to put in the document.
    let plain = matches!(format, OutputFormat::Text | OutputFormat::Sh);
    let mut slots: Vec<(&str, Option<Value>)> = Vec::with_capacity(keys.len());
    let mut any_error = false;
    for key in keys {
        let value = match resolve(key, &inputs, &mut session) {
            Ok(Some(value)) => Some(value),
            Ok(None) => {
                if !plain {
                    eprintln!("Error querying {key}: no data (cache miss)");
                    any_error = true;
                }
                None
            }
            // The daemon's own words for a key it refused, or the transport's.
            Err(ResolveError::Fetch(e)) => {
                eprintln!("Error querying {key}: {e}");
                any_error = true;
                None
            }
            // A config or runtime failure already names its field.
            Err(e) => {
                eprintln!("{e}");
                any_error = true;
                None
            }
        };
        slots.push((key, value));
    }

    let rendered = if keys.len() == 1 {
        render_one(&format, slots[0].1.as_ref())
    } else {
        render_many(&format, &slots)
    };
    match rendered {
        Ok(out) => print!("{out}"),
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    }
    if any_error {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

/// One key's value, or nothing for a miss. `text`/`sh`/`csv`/`tsv`/`fmt` are
/// bare — no trailing newline — so the output embeds verbatim; `json` ends in
/// a newline like every JSON document `comb` writes.
fn render_one(format: &OutputFormat, value: Option<&Value>) -> Result<String, String> {
    let Some(value) = value else {
        return Ok(String::new());
    };
    Ok(match format {
        OutputFormat::Text => render_data(Some(value), "text"),
        OutputFormat::Sh => render_data(Some(value), "sh"),
        OutputFormat::Json => format!("{}\n", serde_json::to_string_pretty(value).unwrap()),
        OutputFormat::Csv => format_sv(value, ",", false),
        OutputFormat::Tsv => format_sv(value, "\t", false),
        OutputFormat::CsvHeader => format_sv(value, ",", true),
        OutputFormat::TsvHeader => format_sv(value, "\t", true),
        OutputFormat::Fmt(template) => render_fmt_template_json(template, value)?,
    })
}

/// Several keys' values as one document, one slot per key: `text`/`sh` one
/// rendering per line (a slot with no value prints nothing), `json` an array
/// in key order (`null`), `csv`/`tsv` one row across every key — an object
/// contributes its sorted fields, a scalar its key, no value an empty cell —
/// and `fmt` a template over the merge of every object's fields, as
/// `provider.field` and as `field`, and every scalar under its key.
fn render_many(format: &OutputFormat, slots: &[(&str, Option<Value>)]) -> Result<String, String> {
    Ok(match format {
        OutputFormat::Text | OutputFormat::Sh => {
            let wire = if matches!(format, OutputFormat::Sh) {
                "sh"
            } else {
                "text"
            };
            slots
                .iter()
                .map(|(_, v)| render_data(v.as_ref(), wire))
                .filter(|line| !line.is_empty())
                .map(|line| format!("{line}\n"))
                .collect()
        }
        OutputFormat::Json => {
            let values: Vec<&Option<Value>> = slots.iter().map(|(_, v)| v).collect();
            format!("{}\n", serde_json::to_string_pretty(&values).unwrap())
        }
        OutputFormat::Csv
        | OutputFormat::Tsv
        | OutputFormat::CsvHeader
        | OutputFormat::TsvHeader => {
            let sep = if matches!(format, OutputFormat::Csv | OutputFormat::CsvHeader) {
                ","
            } else {
                "\t"
            };
            let with_header = matches!(format, OutputFormat::CsvHeader | OutputFormat::TsvHeader);
            let mut names: Vec<String> = Vec::new();
            let mut values: Vec<String> = Vec::new();
            for (key, value) in slots {
                match value {
                    Some(Value::Object(map)) => {
                        let mut pairs: Vec<(&String, &Value)> = map.iter().collect();
                        pairs.sort_by_key(|(k, _)| *k);
                        for (k, v) in pairs {
                            names.push(k.clone());
                            values.push(value_to_string(v));
                        }
                    }
                    Some(value) => {
                        names.push(key.to_string());
                        values.push(value_to_string(value));
                    }
                    None => {
                        names.push(key.to_string());
                        values.push(String::new());
                    }
                }
            }
            let mut out = String::new();
            if with_header {
                out.push_str(&names.join(sep));
                out.push('\n');
            }
            out.push_str(&values.join(sep));
            out.push('\n');
            out
        }
        OutputFormat::Fmt(template) => {
            let mut merged = serde_json::Map::new();
            for (key, value) in slots {
                match value {
                    Some(Value::Object(map)) => {
                        let provider = key.split('.').next().unwrap_or(key);
                        for (k, v) in map {
                            merged.insert(format!("{provider}.{k}"), v.clone());
                            merged.insert(k.clone(), v.clone());
                        }
                    }
                    Some(value) => {
                        merged.insert(key.to_string(), value.clone());
                    }
                    None => {}
                }
            }
            render_fmt_template_json(template, &Value::Object(merged))?
        }
    })
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
    fn slash_containing_is_path_like() {
        assert!(is_path_like("some/dir"));
        assert!(is_path_like("a/b/c"));
    }

    #[test]
    fn provider_key_is_not_path_like() {
        // Keys contain `.` but not `/` or any path prefix.
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
    fn tilde_path_is_popped() {
        let (ks, path) = split_keys_and_path(keys(&["git.branch", "~/myrepo"]), None);
        assert_eq!(ks, vec!["git.branch"]);
        assert_eq!(path.as_deref(), Some("~/myrepo"));
    }
}
