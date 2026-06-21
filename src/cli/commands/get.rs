//! Handler for the `get` (`g`) subcommand.
//!
//! Moved from `src/main.rs` in Task 2.2.

use crate::cli::format::render_fmt_template_json;
use crate::cli::output_format::{OutputFormat, format_sv, value_to_string};
use crate::cli::virtual_fields::{
    EvalContext, Ref, VirtualFields, discover_expression_refs, evaluate_namespace,
};
use crate::config::Config;
use std::collections::HashMap;
use std::process::ExitCode;

/// Compute the cache-key path for a daemon-bound key using its provider's path
/// expression, if one is declared (built-in or config override).
///
/// Returns:
/// - `Some(Some(path))` — a computed, non-empty path.
/// - `Some(None)` — expression evaluated to empty/falsy → global slot.
/// - `None` — no path expression for this provider → keep existing path logic.
fn path_for_key(key: &str, config: &Config) -> Option<Option<String>> {
    let provider = key.split('.').next().unwrap_or("");
    let expr = crate::cli::path_expr::path_expression_for(provider, &config.path_expressions())?;
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let env: HashMap<String, String> = std::env::vars().collect();
    Some(crate::cli::path_expr::evaluate_path(&expr, &cwd, &env))
}

/// Fetch the daemon-backed refs a virtual field's expression needs.
///
/// Cascade semantics (non-strict undefined): a ref that fails to fetch or is
/// absent is simply omitted, so the evaluator treats it as falsy and the
/// cascade falls through to the next term. This is deliberate — a broken
/// daemon dep must not abort an `env.X or provider.field` cascade.
///
/// Bug #6 fix: `force` and `wait` are threaded through so dep fetches honor
/// the same flags as the top-level `comb get --force`/`--wait` invocation.
async fn fetch_daemon_deps(
    key: &str,
    vf: &VirtualFields,
    session: Option<&mut crate::client::ClientSession>,
    force: bool,
    wait: bool,
) -> HashMap<String, serde_json::Value> {
    let Some((provider, field)) = key.split_once('.') else {
        return HashMap::new();
    };
    let expr = vf.expression(provider, field).unwrap_or("");
    let refs = discover_expression_refs(expr);
    let mut dd: HashMap<String, serde_json::Value> = HashMap::new();
    if let Some(session) = session {
        for r in refs {
            match r {
                Ref::Env(_) => {
                    // env.* — never contacts the daemon.
                }
                Ref::CacheField(p, f) => {
                    // Raw cached field: fetch "P.F" and store under "P.F".
                    let dep_key = format!("{p}.{f}");
                    if let Ok(resp) = session.get_with_flags(&dep_key, None, force, wait).await
                        && let Some(data) = resp.data
                    {
                        dd.insert(dep_key, data);
                    }
                }
                Ref::CacheProvider(p) => {
                    // Whole provider object: fetch "P" and store under "P".
                    if let Ok(resp) = session.get_with_flags(&p, None, force, wait).await
                        && let Some(data) = resp.data
                    {
                        dd.insert(p, data);
                    }
                }
                Ref::Resolved(p, f) => {
                    // Resolved field: if not virtual, fetch "P.F".
                    // Virtual resolved refs are evaluated client-side during expression eval.
                    if !vf.is_virtual(&p, &f) {
                        let dep_key = format!("{p}.{f}");
                        if let Ok(resp) = session.get_with_flags(&dep_key, None, force, wait).await
                            && let Some(data) = resp.data
                        {
                            dd.insert(dep_key, data);
                        }
                    }
                }
            }
        }
    }
    dd
}

/// Fetch daemon-backed refs for the union of all virtual fields in a provider namespace.
///
/// Used for bare-provider namespace evaluation: collects the daemon deps
/// needed across ALL virtual fields of the provider so that `evaluate_namespace`
/// can be called once with a fully-populated `daemon_data` map.
///
/// Canon invariant 12: a whole-provider query returns the whole subtree — the
/// provider's daemon-cached fields AND its virtual fields. To guarantee this,
/// the whole daemon provider `provider` is always fetched unconditionally and
/// stored as `daemon_data[provider]`. Virtual expressions may also contribute
/// additional individual field refs; these are fetched on top. The merge in
/// `run_get` then combines daemon fields with virtual fields (virtual wins on
/// key collision).
async fn fetch_daemon_deps_for_namespace(
    provider: &str,
    vf: &VirtualFields,
    session: Option<&mut crate::client::ClientSession>,
    force: bool,
    wait: bool,
) -> HashMap<String, serde_json::Value> {
    let mut dd: HashMap<String, serde_json::Value> = HashMap::new();
    let Some(session) = session else {
        return dd;
    };
    // Always fetch the whole daemon provider object (if it exists) so that
    // daemon-cached fields are available for the merge step. If the provider
    // has no daemon counterpart (e.g. conda, op), this returns nothing and
    // the merge is a no-op — no error.
    if let Ok(resp) = session.get_with_flags(provider, None, force, wait).await
        && let Some(data) = resp.data
    {
        dd.insert(provider.to_string(), data);
    }
    // Also collect the union of individual daemon refs across all virtual fields.
    let mut seen_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for field in vf.fields_for(provider) {
        let expr = vf.expression(provider, &field).unwrap_or("");
        let refs = discover_expression_refs(expr);
        for r in refs {
            match r {
                Ref::Env(_) => {
                    // env.* — never contacts the daemon.
                }
                Ref::CacheField(p, f) => {
                    let dep_key = format!("{p}.{f}");
                    if seen_keys.insert(dep_key.clone())
                        && let Ok(resp) = session.get_with_flags(&dep_key, None, force, wait).await
                        && let Some(data) = resp.data
                    {
                        dd.insert(dep_key, data);
                    }
                }
                Ref::CacheProvider(p) => {
                    if seen_keys.insert(p.clone())
                        && let Ok(resp) = session.get_with_flags(&p, None, force, wait).await
                        && let Some(data) = resp.data
                    {
                        dd.insert(p, data);
                    }
                }
                Ref::Resolved(p, f) => {
                    if !vf.is_virtual(&p, &f) {
                        let dep_key = format!("{p}.{f}");
                        if seen_keys.insert(dep_key.clone())
                            && let Ok(resp) =
                                session.get_with_flags(&dep_key, None, force, wait).await
                            && let Some(data) = resp.data
                        {
                            dd.insert(dep_key, data);
                        }
                    }
                }
            }
        }
    }
    dd
}

/// Returns true if the key is a bare provider name (no dot) whose provider
/// has at least one virtual field defined. Such keys are resolved client-side
/// via namespace evaluation rather than sent to the daemon as whole-provider
/// queries.
fn is_virtual_namespace_key(key: &str, vf: &VirtualFields) -> bool {
    !key.contains('.') && !vf.fields_for(key).is_empty()
}

/// Format an object JSON value as `key=value` lines (sorted), matching the
/// server-side text/sh convention for whole-provider object output.
fn format_object_text(data: &serde_json::Value) -> String {
    let serde_json::Value::Object(map) = data else {
        return value_to_string(data);
    };
    let mut lines: Vec<String> = map
        .iter()
        .flat_map(|(k, v)| {
            if let serde_json::Value::Object(inner) = v {
                inner
                    .iter()
                    .map(|(ik, iv)| {
                        let val = match iv {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        format!("{k}.{ik}={val}")
                    })
                    .collect::<Vec<_>>()
            } else {
                let val = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                vec![format!("{k}={val}")]
            }
        })
        .collect();
    lines.sort();
    let mut out = lines.join("\n");
    out.push('\n');
    out
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
/// env.* keys never need the daemon (canon invariant 15).
/// Dotless keys that match a virtual-namespace provider (one with at least one
/// virtual field) need the daemon only if any of their virtual field expressions
/// reference daemon (non-env, non-virtual) fields; if all virtual fields are
/// pure-env, the daemon is skipped.
/// Dotless keys for daemon-only providers (no virtual fields) always need the daemon.
/// Virtual fields need the daemon only if their expression references daemon
/// (non-env, non-virtual) fields; pure-env virtuals never need the daemon.
/// Plain `provider.field` keys that are not virtual always need the daemon.
///
/// Bug #4 fix: dotless keys were returning false (no dot → early return false).
/// They must return true: a bare provider name is a whole-provider daemon query
/// (unless it is a virtual-namespace provider where all virtual fields are pure-env).
pub fn key_needs_daemon(key: &str, vf: &VirtualFields) -> bool {
    let Some(dot) = key.find('.') else {
        // Dotless key: check if it's a virtual-namespace provider.
        // If it has virtual fields, need daemon only if any field has daemon refs.
        // If it has no virtual fields, it's a whole-provider daemon query: need daemon.
        let virtual_fields = vf.fields_for(key);
        if virtual_fields.is_empty() {
            // Daemon-only provider (e.g. `git`, `hostname`): whole-provider daemon query.
            return true;
        }
        // Virtual-namespace provider: need daemon if any virtual field has daemon refs,
        // or unconditionally because fetch_daemon_deps_for_namespace always fetches the
        // whole provider object (for canon invariant 12 whole-subtree merge).
        return virtual_fields.iter().any(|field| {
            let expr = vf.expression(key, field).unwrap_or("");
            let refs = discover_expression_refs(expr);
            refs.iter().any(|r| match r {
                Ref::Env(_) => false,
                Ref::CacheField(_, _) => true,
                Ref::CacheProvider(_) => true,
                Ref::Resolved(p, f) => !vf.is_virtual(p, f),
            })
        });
    };
    let provider = &key[..dot];
    let field = &key[dot + 1..];
    if provider == "env" {
        // Canon invariant 15: env.* keys never contact the daemon.
        return false;
    }
    if vf.is_virtual(provider, field) {
        let expr = vf.expression(provider, field).unwrap_or("");
        let refs = discover_expression_refs(expr);
        // Needs daemon if any ref requires a daemon fetch.
        // Note: for single-key virtual with daemon refs, run_get does an env-first
        // pass (#7) and only calls ensure_daemon when the env terms don't win.
        // key_needs_daemon is used for: (a) multi-key any_needs_daemon check,
        // (b) routing within multi-key loops, and (c) single-key fetch_daemon_deps guard.
        refs.iter().any(|r| match r {
            Ref::Env(_) => false,
            Ref::CacheField(_, _) => true,
            Ref::CacheProvider(_) => true,
            Ref::Resolved(p, f) => !vf.is_virtual(p, f),
        })
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
    vf: &VirtualFields,
) -> Result<String, String> {
    let dot = key.find('.').ok_or_else(|| format!("invalid key: {key}"))?;
    let provider = &key[..dot];
    let field = &key[dot + 1..];

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

    // Build the virtual field registry once, config overrides win over built-in defaults.
    let vf = VirtualFields::with_config_overrides(config.virtual_fields());

    // Bug #7 fix: for a single virtual key whose expression has daemon refs, we
    // use an env-first lazy strategy — try evaluation with empty daemon_data first.
    // If the env terms win (non-empty result), we return without ever contacting
    // the daemon. Only if the env-only result is empty do we then ensure_daemon,
    // fetch deps, and re-evaluate. This allows e.g. `TF_WORKSPACE=dev comb get
    // terraform.workspace` to work even when the daemon is not running.
    //
    // For all other cases (multi-key, plain daemon key, dotless key) we use the
    // original upfront any_needs_daemon → ensure_daemon logic.
    let is_single_virtual_with_daemon_refs = keys.len() == 1 && {
        let k = &keys[0];
        is_client_side_key(k, &vf) && key_needs_daemon(k, &vf)
    };

    if !is_single_virtual_with_daemon_refs {
        // Original path: ensure_daemon upfront if any key needs it.
        let any_needs_daemon = keys.iter().any(|k| key_needs_daemon(k, &vf));
        if any_needs_daemon && let Err(e) = crate::daemon::ensure_daemon(&socket_path) {
            eprintln!("Failed to start daemon: {e}");
            return ExitCode::from(2);
        }
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        // Single-key shortcut: check client-side first, then delegate to daemon.
        if keys.len() == 1 {
            let key = &keys[0];
            if is_client_side_key(key, &vf) {
                if key_needs_daemon(key, &vf) {
                    // Bug #7: env-first lazy evaluation.
                    // Try with empty daemon_data: only env vars + virtual refs are consulted.
                    let empty_daemon_data = HashMap::new();
                    let env_only_result =
                        evaluate_client_side(key, &format, &empty_daemon_data, &vf);
                    let env_only_is_nonempty = match &env_only_result {
                        Ok(text) => {
                            // A non-empty string or non-null value means the env term won.
                            // For Text/Sh this is the formatted string; for Json it's the
                            // stringified value. An empty string or "null" means fell through.
                            !text.is_empty() && text != "\"\"" && text != "null" && text != "''"
                        }
                        Err(_) => false,
                    };

                    if env_only_is_nonempty {
                        // Env term won — output it and skip the daemon entirely.
                        match env_only_result {
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

                    // Env term was empty — now we need the daemon. Ensure it's running.
                    if let Err(e) = crate::daemon::ensure_daemon(&socket_path) {
                        eprintln!("Failed to start daemon: {e}");
                        return ExitCode::from(2);
                    }

                    // Fetch daemon deps (honoring force/wait — bug #6 fix).
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
                    let daemon_data =
                        fetch_daemon_deps(key, &vf, Some(&mut session), force, wait).await;
                    match evaluate_client_side(key, &format, &daemon_data, &vf) {
                        Ok(text) => {
                            print!("{text}");
                            return ExitCode::SUCCESS;
                        }
                        Err(e) => {
                            eprintln!("Error: {e}");
                            return ExitCode::from(2);
                        }
                    }
                } else {
                    // Pure env/virtual with no daemon refs — evaluate directly.
                    match evaluate_client_side(key, &format, &HashMap::new(), &vf) {
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
        }

        // Step 1.4: bare-provider namespace evaluation.
        //
        // If the single key is a dotless provider name that has virtual fields,
        // evaluate all virtual fields client-side, merge any daemon-provider fields
        // on top (daemon fields lose on key collision), and emit the result.
        // Providers without virtual fields (e.g. `git`, `hostname`) fall through
        // to the daemon path below unchanged.
        if keys.len() == 1 {
            let key = &keys[0];
            if is_virtual_namespace_key(key, &vf) {
                // Fetch daemon deps for all virtual fields of this namespace.
                let daemon_data = if key_needs_daemon(key, &vf) {
                    let client = crate::client::Client::new(socket_path.clone());
                    match client.connect().await {
                        Ok(mut session) => {
                            if let Some(p) = path
                                && let Err(e) = session.set_context(p).await
                            {
                                eprintln!("Error: {e}");
                                return ExitCode::from(2);
                            }
                            fetch_daemon_deps_for_namespace(
                                key,
                                &vf,
                                Some(&mut session),
                                force,
                                wait,
                            )
                            .await
                        }
                        Err(e) => {
                            eprintln!("Error: {e}");
                            return ExitCode::from(2);
                        }
                    }
                } else {
                    HashMap::new()
                };

                // Evaluate all virtual fields as a namespace object.
                let env_vars: HashMap<String, String> = std::env::vars().collect();
                let ns_result = evaluate_namespace(key, &vf, &env_vars, &daemon_data);

                // Merge daemon provider fields with virtual fields (canon invariant 12).
                // fetch_daemon_deps_for_namespace unconditionally fetched the whole daemon
                // provider object and stored it as daemon_data[provider]. Daemon fields go
                // in first; virtual fields overwrite on key collision (virtual wins).
                // Providers with no daemon counterpart (e.g. conda, op) return nothing from
                // the whole-provider fetch, so daemon_data[provider] is absent — merge is a no-op.
                let mut merged = serde_json::Map::new();
                if let Some(serde_json::Value::Object(daemon_map)) = daemon_data.get(key.as_str()) {
                    for (k, v) in daemon_map {
                        merged.insert(k.clone(), v.clone());
                    }
                }
                if let serde_json::Value::Object(ns_map) = ns_result {
                    for (k, v) in ns_map {
                        merged.insert(k, v);
                    }
                }
                let data = serde_json::Value::Object(merged);

                // Render per format, matching the existing convention for object output.
                match &format {
                    OutputFormat::Text | OutputFormat::Sh => {
                        print!("{}", format_object_text(&data));
                        return ExitCode::SUCCESS;
                    }
                    OutputFormat::Json => {
                        println!("{}", serde_json::to_string_pretty(&data).unwrap());
                        return ExitCode::SUCCESS;
                    }
                    OutputFormat::Csv => {
                        print!("{}", format_sv(&data, ",", false));
                        return ExitCode::SUCCESS;
                    }
                    OutputFormat::Tsv => {
                        print!("{}", format_sv(&data, "\t", false));
                        return ExitCode::SUCCESS;
                    }
                    OutputFormat::CsvHeader => {
                        print!("{}", format_sv(&data, ",", true));
                        return ExitCode::SUCCESS;
                    }
                    OutputFormat::TsvHeader => {
                        print!("{}", format_sv(&data, "\t", true));
                        return ExitCode::SUCCESS;
                    }
                    OutputFormat::Fmt(template) => {
                        match render_fmt_template_json(template, &data) {
                            Ok(rendered) => {
                                print!("{}", rendered);
                                return ExitCode::SUCCESS;
                            }
                            Err(e) => {
                                eprintln!("Template error: {e}");
                                return ExitCode::from(2);
                            }
                        }
                    }
                }
            }
        }

        // Single-key shortcut for server-side formats (text / sh): delegate directly to the
        // client helper so the daemon renders the value consistently.
        if keys.len() == 1 && format.is_server_side() {
            let key = &keys[0];
            // Use the path expression result if one is declared for this provider;
            // otherwise fall back to the caller-supplied path.
            let computed_path = path_for_key(key, config);
            let effective_path: Option<&str> = match computed_path {
                Some(ref inner) => inner.as_deref(),
                None => path,
            };
            let client = crate::client::Client::new(socket_path.clone());
            match client
                .get_formatted_with_flags(key, effective_path, format.server_format(), force, wait)
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
        // (any_needs_daemon was already satisfied by ensure_daemon above for the non-single-
        // virtual path; we re-derive it here for the session-open decision.)
        let any_needs_daemon_for_session = keys.iter().any(|k| key_needs_daemon(k, &vf));
        let client = crate::client::Client::new(socket_path.clone());
        let mut session_opt = if any_needs_daemon_for_session {
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
                    // Fetch daemon deps for this virtual key if needed (bug #6: pass force/wait).
                    let daemon_data = if key_needs_daemon(key, &vf) {
                        fetch_daemon_deps(key, &vf, session_opt.as_mut(), force, wait).await
                    } else {
                        HashMap::new()
                    };
                    match evaluate_client_side(key, &format, &daemon_data, &vf) {
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
                    let key_path: Option<Option<String>> = path_for_key(key, config);
                    let effective_key_path: Option<&str> = match key_path {
                        Some(ref computed) => computed.as_deref(),
                        None => None,
                    };
                    match session
                        .get_formatted_with_flags(key, effective_key_path, wire_fmt, force, wait)
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
                // Fetch daemon deps if expression needs them (bug #6: pass force/wait).
                let daemon_data = if key_needs_daemon(key, &vf) {
                    fetch_daemon_deps(key, &vf, session_opt.as_mut(), force, wait).await
                } else {
                    HashMap::new()
                };
                // Evaluate and synthesize a Response-like value for the aggregation path.
                // Multi-key --format json wraps every key (virtual and daemon) uniformly in the
                // response shape, intentionally matching existing daemon multi-key JSON output.
                // Single-key client-side JSON emits the bare value (see single-key path above).
                match evaluate_client_side(key, &OutputFormat::Json, &daemon_data, &vf) {
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
                let key_path: Option<Option<String>> = path_for_key(key, config);
                let effective_key_path: Option<&str> = match key_path {
                    Some(ref computed) => computed.as_deref(),
                    None => None,
                };
                match session
                    .get_with_flags(key, effective_key_path, force, wait)
                    .await
                {
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
