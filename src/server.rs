use crate::cache::Cache;
use crate::protocol::{self, Format, Request, Response};
use crate::provider::registry::ProviderRegistry;
use crate::scheduler::{SchedulerHandle, SchedulerMessage, TriggerSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{info, warn, debug};

pub struct Server {
    socket_path: PathBuf,
    cache: Arc<Cache>,
    registry: Arc<ProviderRegistry>,
    scheduler: Option<SchedulerHandle>,
    next_consumer_id: Arc<AtomicU64>,
}

impl Server {
    pub fn new(
        socket_path: PathBuf,
        cache: Arc<Cache>,
        registry: Arc<ProviderRegistry>,
        scheduler: Option<SchedulerHandle>,
    ) -> Self {
        Self {
            socket_path,
            cache,
            registry,
            scheduler,
            next_consumer_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub async fn run(&self) -> std::io::Result<()> {
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let _ = std::fs::remove_file(&self.socket_path);

        let listener = UnixListener::bind(&self.socket_path)?;
        info!("Listening on {:?}", self.socket_path);

        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let cache = Arc::clone(&self.cache);
                    let registry = Arc::clone(&self.registry);
                    let scheduler = self.scheduler.clone();
                    let consumer_id = self.next_consumer_id.fetch_add(1, Ordering::Relaxed);
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, cache, registry, scheduler, consumer_id).await {
                            debug!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    warn!("Accept error: {}", e);
                }
            }
        }
    }
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    cache: Arc<Cache>,
    registry: Arc<ProviderRegistry>,
    scheduler: Option<SchedulerHandle>,
    consumer_id: u64,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    let mut context_path: Option<String> = None;

    while reader.read_line(&mut line).await? > 0 {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            line.clear();
            continue;
        }

        let response_bytes = match serde_json::from_str::<Request>(trimmed) {
            Ok(request) => {
                let response = handle_request(&request, &cache, &registry, scheduler.as_ref(), consumer_id, &mut context_path).await;
                format_response(&request, &response)
            }
            Err(e) => {
                let resp = Response::error(format!("invalid request: {}", e));
                let mut out = serde_json::to_string(&resp).unwrap();
                out.push('\n');
                out
            }
        };

        writer.write_all(response_bytes.as_bytes()).await?;
        line.clear();
    }

    // Connection closed — notify scheduler of disconnect.
    if let Some(sched) = &scheduler {
        sched.send(SchedulerMessage::ConsumerDisconnected { consumer_id }).await;
    }

    Ok(())
}

/// Resolve the effective path for a request: explicit path > context path > None.
/// Only applies the context for path-scoped (non-global) providers.
fn resolve_path<'a>(
    explicit: Option<&'a str>,
    context: &'a Option<String>,
    provider_name: &str,
    registry: &ProviderRegistry,
) -> Option<String> {
    if explicit.is_some() {
        return explicit.map(|s| s.to_string());
    }
    // Only apply context path for non-global (path-scoped) providers.
    if let Some(provider) = registry.get(provider_name) {
        if !provider.metadata().global {
            return context.clone();
        }
    }
    None
}

async fn handle_request(
    request: &Request,
    cache: &Cache,
    registry: &ProviderRegistry,
    scheduler: Option<&SchedulerHandle>,
    consumer_id: u64,
    context_path: &mut Option<String>,
) -> Response {
    match request {
        Request::Get { key, path, .. } => {
            let (provider_name, field) = protocol::split_key(key);

            if registry.get(provider_name).is_none() {
                return Response::error(format!("unknown provider: {}", provider_name));
            }

            let effective_path = resolve_path(path.as_deref(), context_path, provider_name, registry);

            match cache.get(provider_name, effective_path.as_deref()) {
                Some(entry) => {
                    let age_ms = entry.age_ms();
                    let stale = entry.is_stale();
                    let data = if let Some(field_name) = field {
                        match entry.result.get(field_name) {
                            Some(value) => serde_json::to_value(value).unwrap(),
                            None => return Response::error(
                                format!("unknown field: {}.{}", provider_name, field_name)
                            ),
                        }
                    } else {
                        entry.result.to_json()
                    };
                    Response::ok(data, age_ms, stale)
                }
                None => Response::miss(),
            }
        }
        Request::Poke { key, path } => {
            let (provider_name, _) = protocol::split_key(key);
            let effective_path = resolve_path(path.as_deref(), context_path, provider_name, registry);

            if let Some(sched) = scheduler {
                // Route through scheduler.
                sched.send(SchedulerMessage::Poke {
                    provider: provider_name.to_string(),
                    path: effective_path,
                }).await;
                Response { ok: true, data: None, age_ms: None, stale: None, error: None }
            } else {
                // Fallback: execute directly (used by tests with None scheduler).
                match registry.get(provider_name) {
                    Some(provider) => {
                        if let Some(result) = provider.execute(effective_path.as_deref()) {
                            cache.put(provider_name, effective_path.as_deref(), result);
                        }
                        Response { ok: true, data: None, age_ms: None, stale: None, error: None }
                    }
                    None => Response::error(format!("unknown provider: {}", provider_name)),
                }
            }
        }
        Request::Subscribe { key, path, triggers } => {
            let (provider_name, _) = protocol::split_key(key);
            let effective_path = resolve_path(path.as_deref(), context_path, provider_name, registry);

            if let Some(sched) = scheduler {
                let trigger_set = TriggerSet::from_protocol(triggers);
                sched.send(SchedulerMessage::Subscribe {
                    consumer_id,
                    provider: provider_name.to_string(),
                    path: effective_path,
                    triggers: trigger_set,
                }).await;
            }

            Response { ok: true, data: None, age_ms: None, stale: None, error: None }
        }
        Request::Unsubscribe { key, path } => {
            let (provider_name, _) = protocol::split_key(key);
            let effective_path = resolve_path(path.as_deref(), context_path, provider_name, registry);

            if let Some(sched) = scheduler {
                sched.send(SchedulerMessage::Unsubscribe {
                    consumer_id,
                    provider: provider_name.to_string(),
                    path: effective_path,
                }).await;
            }

            Response { ok: true, data: None, age_ms: None, stale: None, error: None }
        }
        Request::Context { path } => {
            *context_path = Some(path.clone());
            Response { ok: true, data: None, age_ms: None, stale: None, error: None }
        }
        Request::List => {
            let providers: Vec<serde_json::Value> = registry.list().into_iter()
                .map(|name| {
                    let meta = registry.get(&name).unwrap().metadata();
                    serde_json::json!({
                        "name": name,
                        "global": meta.global,
                        "fields": meta.fields.iter().map(|f| &f.name).collect::<Vec<_>>(),
                    })
                })
                .collect();
            Response::ok(serde_json::json!(providers), 0, false)
        }
        Request::Status => {
            let data = serde_json::json!({
                "cache_entries": cache.len(),
                "providers": registry.list().len(),
            });
            Response::ok(data, 0, false)
        }
    }
}

fn format_response(request: &Request, response: &Response) -> String {
    let format = match request {
        Request::Get { format, .. } => format,
        _ => &Format::Json,
    };

    match format {
        Format::Text => {
            if !response.ok {
                return format!("error: {}\n", response.error.as_deref().unwrap_or("unknown"));
            }
            match &response.data {
                Some(serde_json::Value::String(s)) => format!("{}\n", s),
                Some(serde_json::Value::Number(n)) => format!("{}\n", n),
                Some(serde_json::Value::Bool(b)) => format!("{}\n", b),
                Some(serde_json::Value::Object(map)) => {
                    let mut lines: Vec<String> = map.iter()
                        .map(|(k, v)| {
                            let val = match v {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            format!("{}={}", k, val)
                        })
                        .collect();
                    lines.sort();
                    let mut out = lines.join("\n");
                    out.push('\n');
                    out
                }
                Some(serde_json::Value::Null) | None => "\n".to_string(),
                Some(other) => format!("{}\n", other),
            }
        }
        Format::Json => {
            let mut out = serde_json::to_string(response).unwrap();
            out.push('\n');
            out
        }
    }
}
