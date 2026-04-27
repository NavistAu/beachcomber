//! Handler for the `eval` subcommand.
//!
//! Moved from `src/main.rs` in Task 2.6.

use crate::cli::format::{find_eval_template_pairs, render_eval_template};
use crate::config::Config;
use std::process::ExitCode;

pub fn run_eval(config: &Config, template: &str, path: Option<&str>) -> ExitCode {
    let socket_path = config.resolve_socket_path();

    if let Err(e) = crate::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    // Discover (provider, field) pairs referenced in the template.
    let pairs = find_eval_template_pairs(template);

    // If the template has no provider.field references, render it directly
    // (it may still contain jinja conditionals, literals, etc.).
    if pairs.is_empty() {
        return match render_eval_template(template, &serde_json::Value::Object(Default::default()))
        {
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

    // Deduplicate provider.field pairs before querying.
    let mut seen = std::collections::HashSet::new();
    let unique_pairs: Vec<(String, String)> = pairs
        .into_iter()
        .filter(|pair: &(String, String)| seen.insert(pair.clone()))
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

        // Fetch each provider.field and assemble a nested JSON context.
        let mut ctx: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
        for (provider, field) in &unique_pairs {
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
