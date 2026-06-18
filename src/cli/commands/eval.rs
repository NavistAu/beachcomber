//! Handler for the `eval` subcommand.
//!
//! Moved from `src/main.rs` in Task 2.6.

use crate::cli::format::{find_eval_template_pairs, render_eval_template};
use crate::config::Config;
use std::process::ExitCode;

pub fn run_eval(config: &Config, template: &str, path: Option<&str>) -> ExitCode {
    let socket_path = config.resolve_socket_path();

    // Discover (provider, field) pairs referenced in the template.
    let pairs = find_eval_template_pairs(template);

    // Separate env.* refs from daemon-backed refs.
    let mut env_pairs: Vec<(String, String)> = Vec::new();
    let mut daemon_pairs: Vec<(String, String)> = Vec::new();
    for (provider, field) in pairs {
        if provider == "env" {
            env_pairs.push((provider, field));
        } else {
            daemon_pairs.push((provider, field));
        }
    }

    // Build env.* context from the calling shell's environment.
    let shell_env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let mut ctx: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

    // Always inject the env.* object, even if no env.* refs were found.
    // This lets templates use {{ env.FOO | default("") }} without error.
    {
        let mut env_map = serde_json::Map::new();
        for (_, field) in &env_pairs {
            let val = shell_env.get(field).cloned().unwrap_or_default();
            env_map.insert(field.clone(), serde_json::Value::String(val));
        }
        ctx.insert("env".to_string(), serde_json::Value::Object(env_map));
    }

    // If there are no daemon-backed refs, render without contacting the daemon.
    if daemon_pairs.is_empty() {
        return match render_eval_template(template, &serde_json::Value::Object(ctx)) {
            Ok(s) => {
                print!("{s}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("template render error: {e}");
                ExitCode::from(2)
            }
        };
    }

    // There are daemon-backed refs — ensure daemon is running.
    if let Err(e) = crate::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    // Deduplicate daemon pairs.
    let mut seen = std::collections::HashSet::new();
    let unique_daemon_pairs: Vec<(String, String)> = daemon_pairs
        .into_iter()
        .filter(|pair| seen.insert(pair.clone()))
        .collect();

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = crate::client::Client::new(socket_path);
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

        // Fetch each daemon provider.field and assemble into the context.
        for (provider, field) in &unique_daemon_pairs {
            let key = format!("{provider}.{field}");
            match session.get(&key, None).await {
                Ok(response) => {
                    if let Some(data) = &response.data {
                        let provider_entry = ctx
                            .entry(provider.clone())
                            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                        if let serde_json::Value::Object(m) = provider_entry {
                            m.insert(field.clone(), data.clone());
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error querying {key}: {e}");
                    return ExitCode::from(2);
                }
            }
        }

        // Render the template with the nested context.
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
    })
}
