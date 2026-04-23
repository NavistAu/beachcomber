use crate::config::HttpProviderConfig;
use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata, ProviderResult, Value,
};
use tracing::debug;

pub struct HttpProvider {
    name: String,
    config: HttpProviderConfig,
}

impl HttpProvider {
    pub fn new(name: &str, config: HttpProviderConfig) -> Self {
        Self {
            name: name.to_string(),
            config,
        }
    }
}

impl Provider for HttpProvider {
    fn metadata(&self) -> ProviderMetadata {
        let invalidation = build_invalidation(&self.config);

        ProviderMetadata {
            name: self.name.clone(),
            fields: vec![], // Dynamic — fields come from the response
            invalidation,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
        match self.execute_inner() {
            Some(result) => vec![(None, result)],
            None => Vec::new(),
        }
    }
}

impl HttpProvider {
    fn execute_inner(&self) -> Option<ProviderResult> {
        let url = expand_env_vars(&self.config.url);
        let method = self.config.method.as_deref().unwrap_or("GET");

        // Build a list of (key, expanded_value) header pairs
        let header_pairs: Vec<(String, String)> = self
            .config
            .headers
            .as_ref()
            .map(|h| {
                h.iter()
                    .map(|(k, v)| (k.clone(), expand_env_vars(v)))
                    .collect()
            })
            .unwrap_or_default();

        // ureq 3.x uses type-state for body: GET/HEAD/DELETE return RequestBuilder<WithoutBody>,
        // POST/PUT/PATCH return RequestBuilder<WithBody>. We handle them separately.
        let body_str = self.config.body.as_deref().unwrap_or("");
        let response = match method {
            "POST" | "PUT" | "PATCH" => {
                let mut req = match method {
                    "PUT" => ureq::put(&url),
                    "PATCH" => ureq::patch(&url),
                    _ => ureq::post(&url),
                };
                for (key, val) in &header_pairs {
                    req = req.header(key.as_str(), val.as_str());
                }
                req.send(body_str.as_bytes())
            }
            _ => {
                let mut req = ureq::get(&url);
                for (key, val) in &header_pairs {
                    req = req.header(key.as_str(), val.as_str());
                }
                req.call()
            }
        };

        let mut response = match response {
            Ok(resp) => resp,
            Err(e) => {
                debug!("HTTP provider '{}' request failed: {}", self.name, e);
                return None;
            }
        };

        let body = match response.body_mut().read_to_string() {
            Ok(s) => s,
            Err(e) => {
                debug!(
                    "HTTP provider '{}' failed to read response body: {}",
                    self.name, e
                );
                return None;
            }
        };

        // Parse JSON response
        let json: serde_json::Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(_) => {
                // If not JSON, return as a single "body" field
                let mut result = ProviderResult::new();
                result.insert("body", Value::String(body));
                return Some(result);
            }
        };

        // If extract path is specified, navigate into the JSON
        let extracted = if let Some(extract) = &self.config.extract {
            extract_json_path(&json, extract)
        } else {
            json
        };

        // Convert to ProviderResult
        json_to_provider_result(&extracted)
    }
}

/// Expand ${ENV_VAR} references in a string
fn expand_env_vars(s: &str) -> String {
    let mut result = s.to_string();
    // Find all ${...} patterns
    while let Some(start) = result.find("${") {
        if let Some(end) = result[start..].find('}') {
            let var_name = result[start + 2..start + end].to_string();
            let var_value = std::env::var(&var_name).unwrap_or_default();
            result = format!(
                "{}{}{}",
                &result[..start],
                var_value,
                &result[start + end + 1..]
            );
        } else {
            break;
        }
    }
    result
}

/// Navigate a dot-separated path into a JSON value.
/// e.g., "status.indicator" extracts json["status"]["indicator"]
fn extract_json_path(json: &serde_json::Value, path: &str) -> serde_json::Value {
    let mut current = json;
    for segment in path.split('.') {
        match current {
            serde_json::Value::Object(map) => {
                current = match map.get(segment) {
                    Some(v) => v,
                    None => return serde_json::Value::Null,
                };
            }
            serde_json::Value::Array(arr) => {
                if let Ok(idx) = segment.parse::<usize>() {
                    current = match arr.get(idx) {
                        Some(v) => v,
                        None => return serde_json::Value::Null,
                    };
                } else {
                    return serde_json::Value::Null;
                }
            }
            _ => return serde_json::Value::Null,
        }
    }
    current.clone()
}

/// Convert a serde_json::Value to ProviderResult
fn json_to_provider_result(value: &serde_json::Value) -> Option<ProviderResult> {
    let mut result = ProviderResult::new();

    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                let v = match val {
                    serde_json::Value::String(s) => Value::String(s.clone()),
                    serde_json::Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            Value::Int(i)
                        } else if let Some(f) = n.as_f64() {
                            Value::Float(f)
                        } else {
                            Value::String(n.to_string())
                        }
                    }
                    serde_json::Value::Bool(b) => Value::Bool(*b),
                    // Nested objects/arrays: serialize back to string
                    other => Value::String(other.to_string()),
                };
                result.insert(key.clone(), v);
            }
        }
        serde_json::Value::String(s) => {
            result.insert("value", Value::String(s.clone()));
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                result.insert("value", Value::Int(i));
            } else if let Some(f) = n.as_f64() {
                result.insert("value", Value::Float(f));
            }
        }
        serde_json::Value::Bool(b) => {
            result.insert("value", Value::Bool(*b));
        }
        serde_json::Value::Null => return None,
        serde_json::Value::Array(_) => {
            result.insert("value", Value::String(value.to_string()));
        }
    }

    if result.fields.is_empty() {
        return None;
    }
    Some(result)
}

fn build_invalidation(config: &HttpProviderConfig) -> InvalidationStrategy {
    let poll_secs = config
        .invalidation
        .as_ref()
        .and_then(|i| i.poll.as_ref())
        .and_then(|s| crate::scheduler::parse_duration_secs_pub(s));

    match poll_secs {
        Some(secs) => InvalidationStrategy::Poll {
            interval_secs: secs,
            floor_secs: 5, // Don't hammer endpoints faster than every 5s
        },
        None => InvalidationStrategy::Poll {
            interval_secs: 60, // Default: once a minute
            floor_secs: 5,
        },
    }
}

// Suppress unused import warning — FieldSchema/FieldType are part of the public
// provider interface pattern but not used in HttpProvider's dynamic-field approach.
#[allow(dead_code)]
fn _unused_imports(_: FieldSchema, _: FieldType) {}
