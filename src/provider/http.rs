use crate::config::HttpProviderConfig;
use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::sync::OnceLock;
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
        ProviderMetadata {
            name: self.name.clone(),
            sources: vec![self.single_source_meta()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(HttpSingleSource {
            name: self.name.clone(),
            config: self.config.clone(),
            meta: OnceLock::new(),
        })]
    }
}

impl HttpProvider {
    fn single_source_meta(&self) -> SourceMetadata {
        build_source_meta(&self.name, &self.config)
    }
}

struct HttpSingleSource {
    name: String,
    config: HttpProviderConfig,
    /// Cached metadata for the lifetime of this Source instance.
    meta: OnceLock<SourceMetadata>,
}

impl Source for HttpSingleSource {
    fn metadata(&self) -> &SourceMetadata {
        self.meta.get_or_init(|| build_source_meta(&self.name, &self.config))
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        match execute_inner(&self.name, &self.config) {
            Some(result) => result,
            None => SourceResult::new(),
        }
    }
}

fn build_source_meta(name: &str, config: &HttpProviderConfig) -> SourceMetadata {
    let poll_secs = config
        .invalidation
        .as_ref()
        .and_then(|i| i.poll.as_ref())
        .and_then(|s| crate::scheduler::parse_duration_secs_pub(s))
        .unwrap_or(60);

    SourceMetadata {
        name: "endpoint".into(),
        // Dynamic — fields come from the HTTP response.
        fields: vec![FieldSchema {
            name: "<field>".into(),
            field_type: FieldType::String,
        }],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: poll_secs },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig { reattempts: 3, interval_secs: 60 },
        fsevents_reinstate: false,
    }
}

fn execute_inner(provider_name: &str, config: &HttpProviderConfig) -> Option<SourceResult> {
    let url = expand_env_vars(&config.url);
    let method = config.method.as_deref().unwrap_or("GET");

    // Build a list of (key, expanded_value) header pairs
    let header_pairs: Vec<(String, String)> = config
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
    let body_str = config.body.as_deref().unwrap_or("");
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
            debug!("HTTP provider '{}' request failed: {}", provider_name, e);
            return None;
        }
    };

    let body = match response.body_mut().read_to_string() {
        Ok(s) => s,
        Err(e) => {
            debug!(
                "HTTP provider '{}' failed to read response body: {}",
                provider_name, e
            );
            return None;
        }
    };

    // Parse JSON response
    let json: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            // If not JSON, return as a single "body" field
            let mut result = SourceResult::new();
            result.insert("body", Value::String(body));
            return Some(result);
        }
    };

    // If extract path is specified, navigate into the JSON
    let extracted = if let Some(extract) = &config.extract {
        extract_json_path(&json, extract)
    } else {
        json
    };

    // Convert to SourceResult
    json_to_source_result(&extracted)
}

/// Expand ${ENV_VAR} references in a string
fn expand_env_vars(s: &str) -> String {
    let mut result = s.to_string();
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

/// Convert a serde_json::Value to SourceResult
fn json_to_source_result(value: &serde_json::Value) -> Option<SourceResult> {
    let mut result = SourceResult::new();

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

// Suppress unused import warning — FieldSchema/FieldType are part of the public
// provider interface pattern.
#[allow(dead_code)]
fn _unused_imports(_: FieldSchema, _: FieldType) {}
