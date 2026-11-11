//! Handler for the `put` subcommand.
//!
//! Moved from `src/main.rs` in Task 2.4.

use crate::cli::exit::Outcome;
use crate::config::Config;

pub fn run_put(
    config: &Config,
    key: &str,
    data_str: &str,
    ttl: Option<std::time::Duration>,
    persist: bool,
    path: Option<&str>,
) -> Outcome {
    // A ttl is whole seconds of at least one; refused here, before a daemon
    // is started for a put that cannot happen.
    let ttl = match ttl {
        Some(d) => match libbeachcomber::duration::whole_seconds(d) {
            Ok(secs) => Some(format!("{secs}s")),
            Err(e) => {
                crate::cli::report::error("--ttl", &e);
                return Outcome::Error;
            }
        },
        None => None,
    };
    let socket_path = match crate::cli::connect::connect(config) {
        Ok(socket) => socket,
        Err(outcome) => return outcome,
    };

    let client = crate::client::Client::new(socket_path);

    let data: serde_json::Value = match serde_json::from_str(data_str) {
        Ok(v) => v,
        Err(e) => {
            crate::cli::report::error(key, format_args!("invalid JSON: {e}"));
            return Outcome::Error;
        }
    };

    // Whether this JSON is legal here depends on the node the key
    // addresses — an object at the root, anything below it — which only the
    // daemon knows. It reports the mismatch.
    let removes = data.is_null();
    match client.put(key, data, ttl.as_deref(), path, persist) {
        Ok(response) => {
            if response.ok {
                // The daemon's own warnings (what the put dropped), then
                // what it now hides or is hidden by.
                for line in &response.log {
                    crate::cli::report::warning(&line.msg);
                }
                if !removes {
                    for warning in layer_warnings(&client, key, path) {
                        crate::cli::report::warning(warning);
                    }
                }
                Outcome::Done
            } else {
                crate::cli::report::error(key, response.error.unwrap_or_default());
                Outcome::Error
            }
        }
        Err(e) => {
            crate::cli::report::daemon_unreachable(e);
            Outcome::Error
        }
    }
}

/// What a put of `key` now hides, or is hidden by, as its `layers` say: at
/// each leaf, a put that answers over a Source's record hides it; a template
/// (or its memo) that answers over a put leaves the put unread. A key the
/// `layers` op does not take (a facet's) says nothing.
fn layer_warnings(client: &crate::client::Client, key: &str, path: Option<&str>) -> Vec<String> {
    let Some(layers) = client
        .connect()
        .ok()
        .and_then(|mut session| session.layers(key, path).ok().flatten())
    else {
        return Vec::new();
    };
    let home = libbeachcomber::path::Home::from_env();
    let mut out = Vec::new();
    let mut leaves = Vec::new();
    stacks(key, &layers, &mut leaves);
    for (leaf, stack) in leaves {
        let label = |r: &serde_json::Value| {
            r.get("facet")
                .or_else(|| r.get("label"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?")
                .to_string()
        };
        let at = |r: &serde_json::Value| {
            r.get("path")
                .and_then(serde_json::Value::as_str)
                .map_or_else(|| "~".to_string(), |p| home.compact(p))
        };
        let Some(answered) = stack
            .iter()
            .find(|r| r.get("answered") == Some(&true.into()))
        else {
            continue;
        };
        match label(answered).as_str() {
            "put" => {
                for hidden in stack.iter().filter(|r| label(r).starts_with("source(")) {
                    out.push(format!(
                        "{leaf}: the put hides {} at {}",
                        label(hidden),
                        at(hidden)
                    ));
                }
            }
            "template" | "memo" if stack.iter().any(|r| label(r) == "put") => {
                out.push(format!(
                    "{leaf}: a template answers here, over the put: a read never sees it"
                ));
            }
            _ => {}
        }
    }
    out
}

/// Every leaf's stack in a `layers` answer, by its full key.
fn stacks<'v>(
    key: &str,
    node: &'v serde_json::Value,
    out: &mut Vec<(String, &'v [serde_json::Value])>,
) {
    match node {
        serde_json::Value::Array(stack) => out.push((key.to_string(), stack)),
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                stacks(&libbeachcomber::key::push(key, k), v, out);
            }
        }
        _ => {}
    }
}
