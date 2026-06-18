//! Handler for the `get` (`g`) subcommand.
//!
//! Moved from `src/main.rs` in Task 2.2.

use crate::cli::format::render_fmt_template_json;
use crate::cli::output_format::{OutputFormat, format_sv, value_to_string};
use crate::cli::virtual_fields::{EvalContext, VirtualFields, discover_expression_refs};
use crate::config::Config;
use std::collections::HashMap;
use std::process::ExitCode;

/// Fetch the daemon-backed refs a virtual field's expression needs.
///
/// Cascade semantics (non-strict undefined): a ref that fails to fetch or is
/// absent is simply omitted, so the evaluator treats it as falsy and the
/// cascade falls through to the next term. This is deliberate — a broken
/// daemon dep must not abort an `env.X or provider.field` cascade.
async fn fetch_daemon_deps(
    key: &str,
    vf: &VirtualFields,
    session: Option<&mut crate::client::ClientSession>,
) -> HashMap<String, serde_json::Value> {
    let Some((provider, field)) = key.split_once('.') else {
        return HashMap::new();
    };
    let expr = vf.expression(provider, field).unwrap_or("");
    let refs = discover_expression_refs(expr);
    let mut dd: HashMap<String, serde_json::Value> = HashMap::new();
    if let Some(session) = session {
        for (p, f) in refs {
            if p != "env" && !vf.is_virtual(&p, &f) {
                let dep_key = format!("{p}.{f}");
                if let Ok(resp) = session.get(&dep_key, None).await
                    && let Some(data) = resp.data
                {
                    dd.insert(dep_key, data);
                }
            }
        }
    }
    dd
}

/// Returns true if a `provider.field` key should be resolved client-side
/// (virtual field or env.* namespace) rather than forwarded to the daemon.
fn is_client_side_key(key: &str, vf: &VirtualFields) -> bool {
    let Some(dot) = key.find('.') else {
        return false;
    };
    let provider = &key[..dot];
    let field = &key[dot + 1..];
    provider == "env" || vf.is_virtual(provider, field)
}

/// Returns true if the key requires the daemon to be running.
///
/// env.* keys never need the daemon. Virtual fields only need the daemon
/// if their expression references non-env, non-virtual fields.
/// Plain daemon fields always need the daemon.
fn key_needs_daemon(key: &str, vf: &VirtualFields) -> bool {
    let Some(dot) = key.find('.') else {
        return false;
    };
    let provider = &key[..dot];
    let field = &key[dot + 1..];
    if provider == "env" {
        return false;
    }
    if vf.is_virtual(provider, field) {
        let expr = vf.expression(provider, field).unwrap_or("");
        let refs = discover_expression_refs(expr);
        // Needs daemon if any ref is a non-env, non-virtual field.
        refs.iter().any(|(p, f)| p != "env" && !vf.is_virtual(p, f))
    } else {
        true // plain daemon field
    }
}

/// Format a virtual/env field value for the given output format.
fn format_virtual_value(val: &serde_json::Value, format: &OutputFormat) -> Result<String, String> {
    match format {
        OutputFormat::Text => Ok(value_to_string(val)),
        OutputFormat::Sh => {
            // POSIX single-quote escape: wrap in single quotes, replace each ' with '\''
            let s = value_to_string(val);
            let escaped = s.replace('\'', r#"'\''"#);
            Ok(format!("'{escaped}'"))
        }
        OutputFormat::Json => {
            // For virtual fields, emit just the value (no age/stale wrapper).
            serde_json::to_string_pretty(val).map_err(|e| e.to_string())
        }
        _ => Ok(value_to_string(val)),
    }
}

/// Evaluate a client-side key (virtual field or env.*) and format its value.
///
/// `daemon_data` must contain pre-fetched values for any daemon-backed refs
/// the expression needs (keyed as `"provider.field"`).
///
/// Returns `Ok(formatted_string)` or `Err(error_message)`.
fn evaluate_client_side(
    key: &str,
    format: &OutputFormat,
    daemon_data: &HashMap<String, serde_json::Value>,
) -> Result<String, String> {
    let dot = key.find('.').ok_or_else(|| format!("invalid key: {key}"))?;
    let provider = &key[..dot];
    let field = &key[dot + 1..];

    let vf = VirtualFields::defaults_only(); // TODO Task 6: pass config overrides
    let env_vars: HashMap<String, String> = std::env::vars().collect();

    if provider == "env" {
        let val = env_vars.get(field).cloned().unwrap_or_default();
        let json_val = serde_json::Value::String(val);
        return format_virtual_value(&json_val, format);
    }

    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data,
    };
    let json_val = vf.evaluate(provider, field, &ctx, &mut Default::default())?;
    format_virtual_value(&json_val, format)
}

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

    // Build the virtual field registry once. TODO Task 6: load from config.
    let vf = VirtualFields::defaults_only();

    // Skip ensure_daemon if all keys are client-side and need no daemon deps.
    let any_needs_daemon = keys.iter().any(|k| key_needs_daemon(k, &vf));
    if any_needs_daemon && let Err(e) = crate::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        // Single-key shortcut: check client-side first, then delegate to daemon.
        if keys.len() == 1 {
            let key = &keys[0];
            if is_client_side_key(key, &vf) {
                // Fetch any daemon-backed refs the expression needs.
                let daemon_data = if key_needs_daemon(key, &vf) {
                    // Expression has daemon refs — fetch them via the daemon.
                    let client = crate::client::Client::new(socket_path.clone());
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
                    fetch_daemon_deps(key, &vf, Some(&mut session)).await
                } else {
                    HashMap::new()
                };
                match evaluate_client_side(key, &format, &daemon_data) {
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
        }

        // Single-key shortcut for server-side formats (text / sh): delegate directly to the
        // client helper so the daemon renders the value consistently.
        if keys.len() == 1 && format.is_server_side() {
            let key = &keys[0];
            let client = crate::client::Client::new(socket_path.clone());
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
        // Only connect to the daemon when at least one key actually requires it.
        let client = crate::client::Client::new(socket_path.clone());
        let mut session_opt = if any_needs_daemon {
            match client.connect().await {
                Ok(s) => Some(s),
                Err(e) => {
                    eprintln!("Error: {e}");
                    return ExitCode::from(2);
                }
            }
        } else {
            None
        };

        if let Some(ref mut session) = session_opt
            && let Some(p) = path
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
                if is_client_side_key(key, &vf) {
                    // Fetch daemon deps for this virtual key if needed.
                    let daemon_data = if key_needs_daemon(key, &vf) {
                        fetch_daemon_deps(key, &vf, session_opt.as_mut()).await
                    } else {
                        HashMap::new()
                    };
                    match evaluate_client_side(key, &format, &daemon_data) {
                        Ok(text) => {
                            if !text.is_empty() {
                                println!("{text}");
                            }
                        }
                        Err(e) => {
                            eprintln!("Error: {e}");
                            any_error = true;
                        }
                    }
                    continue;
                }
                if let Some(ref mut session) = session_opt {
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
                } else {
                    eprintln!("Error querying {key}: daemon not available");
                    any_error = true;
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
        // Client-side keys (env.* / virtual) are evaluated without daemon contact.
        let mut responses: Vec<(String, crate::protocol::Response)> = Vec::new();
        let mut any_error = false;
        for key in keys {
            if is_client_side_key(key, &vf) {
                // Fetch daemon deps if expression needs them.
                let daemon_data = if key_needs_daemon(key, &vf) {
                    fetch_daemon_deps(key, &vf, session_opt.as_mut()).await
                } else {
                    HashMap::new()
                };
                // Evaluate and synthesize a Response-like value for the aggregation path.
                // Multi-key --format json wraps every key (virtual and daemon) uniformly in the
                // response shape, intentionally matching existing daemon multi-key JSON output.
                // Single-key client-side JSON emits the bare value (see single-key path above).
                match evaluate_client_side(key, &OutputFormat::Json, &daemon_data) {
                    Ok(json_str) => {
                        let data: serde_json::Value =
                            serde_json::from_str(&json_str).unwrap_or(serde_json::Value::Null);
                        let response = crate::protocol::Response {
                            ok: true,
                            data: Some(data),
                            error: None,
                            age_ms: None,
                            stale: None,
                        };
                        responses.push((key.clone(), response));
                    }
                    Err(e) => {
                        eprintln!("Error querying {key}: {e}");
                        any_error = true;
                    }
                }
                continue;
            }
            if let Some(ref mut session) = session_opt {
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
            } else {
                eprintln!("Error querying {key}: daemon not available");
                any_error = true;
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
