//! Handler for the `eval` subcommand.
//!
//! Moved from `src/main.rs` in Task 2.6.
//!
//! `eval` renders a MiniJinja template referencing `provider.field` values.
//! Resolution mirrors `comb get`'s layering:
//!
//! - `env.*` refs come from the calling shell's environment.
//! - virtual fields (e.g. `terraform.workspace`) are evaluated client-side.
//! - plain `provider.field` refs are fetched from the daemon.
//!
//! A template that needs no daemon-backed data never starts/contacts the daemon.

use crate::cli::format::{find_eval_template_pairs, render_eval_template};
use crate::cli::virtual_fields::{EvalContext, Ref, VirtualFields, discover_expression_refs};
use crate::config::Config;
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;

pub fn run_eval(config: &Config, template: &str, path: Option<&str>) -> ExitCode {
    let (socket_path, socket_source) = config.resolve_socket_path_with_source();
    let spawn_no_reap = matches!(socket_source, crate::config::SocketPathSource::EnvVar);
    let vf = VirtualFields::with_config_overrides(config.virtual_fields());

    // Discover every (provider, field) ref in the template (all tags, all refs).
    let pairs = find_eval_template_pairs(template);

    // Partition refs: env.* (shell), virtual fields (client-side), plain daemon.
    let mut env_fields: Vec<String> = Vec::new();
    let mut virtual_refs: Vec<(String, String)> = Vec::new();
    let mut plain_daemon_refs: Vec<(String, String)> = Vec::new();
    let mut seen_virtual: HashSet<(String, String)> = HashSet::new();
    let mut seen_plain: HashSet<(String, String)> = HashSet::new();
    for (provider, field) in pairs {
        if provider == "env" {
            env_fields.push(field);
        } else if vf.is_virtual(&provider, &field) {
            if seen_virtual.insert((provider.clone(), field.clone())) {
                virtual_refs.push((provider, field));
            }
        } else if seen_plain.insert((provider.clone(), field.clone())) {
            plain_daemon_refs.push((provider, field));
        }
    }

    // Daemon refs to fetch = plain daemon refs ∪ daemon deps of each virtual ref.
    // Stored as typed Ref variants so dispatch at fetch time needs no sentinel encoding.
    let mut daemon_refs: Vec<Ref> = Vec::new();
    let mut seen_field: HashSet<(String, String)> = HashSet::new();
    let mut seen_provider: HashSet<String> = HashSet::new();
    for (p, f) in &plain_daemon_refs {
        if seen_field.insert((p.clone(), f.clone())) {
            daemon_refs.push(Ref::CacheField(p.clone(), f.clone()));
        }
    }
    for (p, f) in &virtual_refs {
        if let Some(expr) = vf.expression(p, f) {
            for r in discover_expression_refs(expr) {
                match r {
                    Ref::Env(_) => {
                        // env.* — no daemon fetch needed.
                    }
                    Ref::CacheField(dp, df) => {
                        if seen_field.insert((dp.clone(), df.clone())) {
                            daemon_refs.push(Ref::CacheField(dp, df));
                        }
                    }
                    Ref::CacheProvider(dp) => {
                        // Whole provider object fetch.
                        if seen_provider.insert(dp.clone()) {
                            daemon_refs.push(Ref::CacheProvider(dp));
                        }
                    }
                    Ref::Resolved(dp, df) => {
                        if !vf.is_virtual(&dp, &df) && seen_field.insert((dp.clone(), df.clone())) {
                            daemon_refs.push(Ref::CacheField(dp, df));
                        }
                    }
                }
            }
        }
    }

    // The calling shell's environment.
    let shell_env: HashMap<String, String> = std::env::vars().collect();

    // Build the render context. Always inject the env.* object (even if empty)
    // so templates can use `{{ env.FOO | default("") }}` without error.
    let mut ctx: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    {
        let mut env_map = serde_json::Map::new();
        for field in &env_fields {
            let val = shell_env.get(field).cloned().unwrap_or_default();
            env_map.insert(field.clone(), serde_json::Value::String(val));
        }
        ctx.insert("env".to_string(), serde_json::Value::Object(env_map));
    }

    // Fetch daemon-backed data only when something actually needs it. A template
    // referencing only env.* and/or pure-env virtual fields never starts the daemon.
    let daemon_data: HashMap<String, serde_json::Value> = if daemon_refs.is_empty() {
        HashMap::new()
    } else {
        if let Err(e) = crate::daemon::ensure_daemon(&socket_path, spawn_no_reap) {
            eprintln!("Failed to start daemon: {e}");
            return ExitCode::from(2);
        }
        let socket_path = socket_path.clone();
        match (|| {
            let client = crate::client::Client::new(socket_path);
            let mut session = match client.connect() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return Err(ExitCode::from(2));
                }
            };
            if let Some(p) = path
                && let Err(e) = session.set_context(p)
            {
                eprintln!("Error: {e}");
                return Err(ExitCode::from(2));
            }
            let mut dd: HashMap<String, serde_json::Value> = HashMap::new();
            for r in &daemon_refs {
                // Dispatch directly on the Ref variant — no sentinel encoding needed.
                let (key, store_key) = match r {
                    Ref::CacheProvider(p) => (p.clone(), p.clone()),
                    Ref::CacheField(p, f) => {
                        let k = format!("{p}.{f}");
                        (k.clone(), k)
                    }
                    // Env and Resolved variants are never added to daemon_refs.
                    _ => continue,
                };
                match session.get(&key, None) {
                    Ok(resp) => {
                        if let Some(data) = resp.data {
                            dd.insert(store_key, data);
                        }
                    }
                    Err(e) => {
                        eprintln!("Error querying {key}: {e}");
                        return Err(ExitCode::from(2));
                    }
                }
            }
            Ok(dd)
        })() {
            Ok(dd) => dd,
            Err(code) => return code,
        }
    };

    // Inject plain daemon refs into the context (nested as ctx[provider][field]).
    for (p, f) in &plain_daemon_refs {
        if let Some(v) = daemon_data.get(&format!("{p}.{f}")) {
            inject(&mut ctx, p, f, v.clone());
        }
    }

    // Evaluate virtual fields client-side and inject their typed results.
    let eval_ctx = EvalContext {
        env_vars: &shell_env,
        daemon_data: &daemon_data,
    };
    for (p, f) in &virtual_refs {
        match vf.evaluate(p, f, &eval_ctx, &mut HashSet::new()) {
            Ok(v) => inject(&mut ctx, p, f, v),
            Err(e) => {
                eprintln!("Error evaluating {p}.{f}: {e}");
                return ExitCode::from(2);
            }
        }
    }

    match render_eval_template(template, &serde_json::Value::Object(ctx)) {
        Ok(s) => {
            print!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("template render error: {e}");
            ExitCode::from(2)
        }
    }
}

/// Insert `value` at the nested context path `ctx[provider][field]`.
fn inject(
    ctx: &mut serde_json::Map<String, serde_json::Value>,
    provider: &str,
    field: &str,
    value: serde_json::Value,
) {
    let entry = ctx
        .entry(provider.to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let serde_json::Value::Object(m) = entry {
        m.insert(field.to_string(), value);
    }
}
