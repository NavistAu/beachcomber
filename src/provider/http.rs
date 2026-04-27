use crate::config::{ExternalSourceConfig, HttpProviderConfig};
use crate::provider::script::build_source_meta_from_external;
use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::collections::HashMap;
use std::sync::OnceLock;
use tracing::debug;

// ── HttpProvider ───────────────────────────────────────────────────────────────
//
// Two construction paths:
//
// 1. `HttpProvider::new(name, HttpProviderConfig)` — single-source, backward
//    compatible. Used by old `type = "http"` TOML and existing tests.
//
// 2. `HttpProvider::with_sources(name, Vec<ExternalSourceConfig>)` — multi-source.
//    Used by Phase 4 `backend = "http"` TOML.

struct HttpSourceEntry {
    meta: SourceMetadata,
    url: String,
    method: Option<String>,
    headers: Option<HashMap<String, String>>,
    body: Option<String>,
    extract: Option<String>,
}

pub struct HttpProvider {
    name: String,
    entries: Vec<HttpSourceEntry>,
}

impl HttpProvider {
    /// Single-source backward-compatible constructor.
    pub fn new(name: &str, config: HttpProviderConfig) -> Self {
        let meta = build_source_meta_legacy(name, &config);
        let entry = HttpSourceEntry {
            url: config.url.clone(),
            method: config.method.clone(),
            headers: config.headers.clone(),
            body: config.body.clone(),
            extract: config.extract.clone(),
            meta,
        };
        Self {
            name: name.to_string(),
            entries: vec![entry],
        }
    }

    /// Multi-source constructor from Phase 4 per-source ExternalSourceConfig list.
    pub fn with_sources(name: &str, source_configs: Vec<ExternalSourceConfig>) -> Self {
        let entries = source_configs
            .into_iter()
            .map(|cfg| {
                let meta = build_source_meta_from_external(&cfg);
                HttpSourceEntry {
                    url: cfg.url.clone().unwrap_or_default(),
                    method: cfg.method.clone(),
                    headers: cfg.headers.clone(),
                    body: cfg.body.clone(),
                    extract: cfg.extract.clone(),
                    meta,
                }
            })
            .collect();
        Self {
            name: name.to_string(),
            entries,
        }
    }
}

impl Provider for HttpProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: self.name.clone(),
            sources: self.entries.iter().map(|e| e.meta.clone()).collect(),
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        self.entries
            .iter()
            .map(|e| {
                Box::new(HttpSingleSource {
                    provider_name: self.name.clone(),
                    url: e.url.clone(),
                    method: e.method.clone(),
                    headers: e.headers.clone(),
                    body: e.body.clone(),
                    extract: e.extract.clone(),
                    meta: OnceLock::new(),
                    meta_value: e.meta.clone(),
                }) as Box<dyn Source>
            })
            .collect()
    }
}

// ── HttpSingleSource ───────────────────────────────────────────────────────────

struct HttpSingleSource {
    provider_name: String,
    url: String,
    method: Option<String>,
    headers: Option<HashMap<String, String>>,
    body: Option<String>,
    extract: Option<String>,
    meta: OnceLock<SourceMetadata>,
    meta_value: SourceMetadata,
}

impl Source for HttpSingleSource {
    fn metadata(&self) -> &SourceMetadata {
        self.meta.get_or_init(|| self.meta_value.clone())
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        match execute_inner(
            &self.provider_name,
            &self.url,
            self.method.as_deref(),
            self.headers.as_ref(),
            self.body.as_deref(),
            self.extract.as_deref(),
        ) {
            Some(result) => result,
            None => SourceResult::new(),
        }
    }
}

// ── Metadata builders ─────────────────────────────────────────────────────────

fn build_source_meta_legacy(_name: &str, config: &HttpProviderConfig) -> SourceMetadata {
    let poll_secs = config
        .invalidation
        .as_ref()
        .and_then(|i| i.poll.as_ref())
        .and_then(|s| crate::scheduler::parse_duration_secs_pub(s))
        .unwrap_or(60);

    SourceMetadata {
        name: "endpoint".into(),
        fields: vec![FieldSchema {
            name: "<field>".into(),
            field_type: FieldType::String,
        }],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll {
            interval_secs: poll_secs,
        },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: false,
    }
}

// ── Execution ─────────────────────────────────────────────────────────────────

fn execute_inner(
    provider_name: &str,
    url_template: &str,
    method: Option<&str>,
    headers: Option<&HashMap<String, String>>,
    body: Option<&str>,
    extract: Option<&str>,
) -> Option<SourceResult> {
    let url = expand_env_vars(url_template);
    let method = method.unwrap_or("GET");

    let header_pairs: Vec<(String, String)> = headers
        .map(|h| {
            h.iter()
                .map(|(k, v)| (k.clone(), expand_env_vars(v)))
                .collect()
        })
        .unwrap_or_default();

    let body_str = body.unwrap_or("");
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

    let body_resp = match response.body_mut().read_to_string() {
        Ok(s) => s,
        Err(e) => {
            debug!(
                "HTTP provider '{}' failed to read response body: {}",
                provider_name, e
            );
            return None;
        }
    };

    let json: serde_json::Value = match serde_json::from_str(&body_resp) {
        Ok(v) => v,
        Err(_) => {
            let mut result = SourceResult::new();
            result.insert("body", Value::String(body_resp));
            return Some(result);
        }
    };

    let extracted = if let Some(extract_path) = extract {
        extract_json_path(&json, extract_path)
    } else {
        json
    };

    json_to_source_result(&extracted)
}

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
