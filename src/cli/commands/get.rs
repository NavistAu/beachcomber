//! Handler for the `get` (`g`) subcommand.
//!
//! Moved from `src/main.rs` in Task 2.2.

use crate::cli::format::render_fmt_template_json;
use crate::cli::output_format::{OutputFormat, format_sv, value_to_string};
use crate::config::Config;
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

pub fn run_get(
    config: &Config,
    keys: &[String],
    path: Option<&str>,
    format: OutputFormat,
    force: bool,
    wait: bool,
) -> ExitCode {
    let socket_path = config.resolve_socket_path();

    if let Err(e) = crate::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = crate::client::Client::new(socket_path.clone());

        // Single-key shortcut for server-side formats (text / sh): delegate directly to the
        // client helper so the daemon renders the value consistently.
        if keys.len() == 1 && format.is_server_side() {
            let key = &keys[0];
            match client
                .get_formatted_with_flags(key, path, format.server_format(), force, wait)
                .await
            {
                Ok(text) => {
                    print!("{text}");
                    return ExitCode::SUCCESS;
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    return ExitCode::from(2);
                }
            }
        }

        // Multi-key (or single-key with client-side format): open one session and issue one
        // Request::Get per key.  Results are aggregated before rendering so that formats like
        // JSON / CSV / TSV can produce a single coherent output document.
        let mut session = match client.connect().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: {e}");
                return ExitCode::from(2);
            }
        };

        if let Some(p) = path
            && let Err(e) = session.set_context(p).await
        {
            eprintln!("Error: {e}");
            return ExitCode::from(2);
        }

        // Server-side formats (text / sh) for multi-key: emit each key's output on its own line.
        if format.is_server_side() {
            let wire_fmt = format.server_format();
            let mut any_error = false;
            for key in keys {
                match session
                    .get_formatted_with_flags(key, None, wire_fmt, force, wait)
                    .await
                {
                    Ok(text) => {
                        if !text.is_empty() {
                            println!("{text}");
                        }
                    }
                    Err(e) => {
                        eprintln!("Error querying {key}: {e}");
                        any_error = true;
                    }
                }
            }
            return if any_error {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            };
        }

        // Client-side formats: collect all responses (preserving per-key association), then
        // render.  Errors are recorded but do not prevent successful keys from being emitted.
        let mut responses: Vec<(String, crate::protocol::Response)> = Vec::new();
        let mut any_error = false;
        for key in keys {
            match session.get_with_flags(key, None, force, wait).await {
                Ok(response) => {
                    if !response.ok {
                        eprintln!(
                            "Error querying {key}: {}",
                            response.error.as_deref().unwrap_or("unknown error")
                        );
                        any_error = true;
                    } else {
                        responses.push((key.clone(), response));
                    }
                }
                Err(e) => {
                    eprintln!("Error querying {key}: {e}");
                    any_error = true;
                }
            }
        }

        // Single-key client-side rendering: preserve the original single-key output shape.
        if keys.len() == 1 {
            if let Some((_, response)) = responses.first() {
                if let Some(data) = &response.data {
                    match &format {
                        OutputFormat::Json => {
                            println!("{}", serde_json::to_string_pretty(response).unwrap());
                        }
                        OutputFormat::Csv => {
                            print!("{}", format_sv(data, ",", false));
                        }
                        OutputFormat::Tsv => {
                            print!("{}", format_sv(data, "\t", false));
                        }
                        OutputFormat::CsvHeader => {
                            print!("{}", format_sv(data, ",", true));
                        }
                        OutputFormat::TsvHeader => {
                            print!("{}", format_sv(data, "\t", true));
                        }
                        OutputFormat::Fmt(template) => {
                            match render_fmt_template_json(template, data) {
                                Ok(rendered) => print!("{}", rendered),
                                Err(e) => {
                                    eprintln!("Template error: {e}");
                                    return ExitCode::from(2);
                                }
                            }
                        }
                        _ => unreachable!(),
                    }
                } else {
                    any_error = true;
                }
            }
            return if any_error {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            };
        }

        // Multi-key client-side aggregation.
        match &format {
            OutputFormat::Json => {
                let arr: Vec<&crate::protocol::Response> =
                    responses.iter().map(|(_, r)| r).collect();
                println!("{}", serde_json::to_string_pretty(&arr).unwrap());
            }
            OutputFormat::Csv | OutputFormat::CsvHeader => {
                let with_header = matches!(format, OutputFormat::CsvHeader);
                let mut all_keys: Vec<String> = Vec::new();
                let mut all_vals: Vec<String> = Vec::new();
                for (key, resp) in &responses {
                    if let Some(data) = &resp.data {
                        match data {
                            serde_json::Value::Object(map) => {
                                let mut pairs: Vec<(&String, &serde_json::Value)> =
                                    map.iter().collect();
                                pairs.sort_by_key(|(k, _)| *k);
                                for (k, v) in pairs {
                                    all_keys.push(k.clone());
                                    all_vals.push(value_to_string(v));
                                }
                            }
                            _ => {
                                all_keys.push(key.clone());
                                all_vals.push(value_to_string(data));
                            }
                        }
                    }
                }
                if with_header {
                    println!("{}", all_keys.join(","));
                }
                println!("{}", all_vals.join(","));
            }
            OutputFormat::Tsv | OutputFormat::TsvHeader => {
                let with_header = matches!(format, OutputFormat::TsvHeader);
                let mut all_keys: Vec<String> = Vec::new();
                let mut all_vals: Vec<String> = Vec::new();
                for (key, resp) in &responses {
                    if let Some(data) = &resp.data {
                        match data {
                            serde_json::Value::Object(map) => {
                                let mut pairs: Vec<(&String, &serde_json::Value)> =
                                    map.iter().collect();
                                pairs.sort_by_key(|(k, _)| *k);
                                for (k, v) in pairs {
                                    all_keys.push(k.clone());
                                    all_vals.push(value_to_string(v));
                                }
                            }
                            _ => {
                                all_keys.push(key.clone());
                                all_vals.push(value_to_string(data));
                            }
                        }
                    }
                }
                if with_header {
                    println!("{}", all_keys.join("\t"));
                }
                println!("{}", all_vals.join("\t"));
            }
            OutputFormat::Fmt(template) => {
                let mut merged = serde_json::Map::new();
                for (key, resp) in &responses {
                    if let Some(data) = &resp.data {
                        match data {
                            serde_json::Value::Object(map) => {
                                let provider = key.split('.').next().unwrap_or(key);
                                for (k, v) in map {
                                    merged.insert(format!("{provider}.{k}"), v.clone());
                                    merged.insert(k.clone(), v.clone());
                                }
                            }
                            _ => {
                                merged.insert(key.clone(), data.clone());
                            }
                        }
                    }
                }
                match render_fmt_template_json(template, &serde_json::Value::Object(merged)) {
                    Ok(rendered) => print!("{}", rendered),
                    Err(e) => {
                        eprintln!("Template error: {e}");
                        return ExitCode::from(2);
                    }
                }
            }
            // Text and Sh multi-key handled above via server-side path.
            OutputFormat::Text | OutputFormat::Sh => unreachable!(),
        }

        if any_error {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
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
