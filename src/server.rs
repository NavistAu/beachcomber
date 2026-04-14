use crate::cache::Cache;
use crate::protocol::{self, Format, Request, Response};
use crate::provider::registry::ProviderRegistry;
use crate::scheduler::{SchedulerHandle, SchedulerMessage};
use crate::watcher_registry::WatcherRegistry;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{debug, info, warn};

pub struct Server {
    socket_path: PathBuf,
    cache: Arc<Cache>,
    registry: Arc<ProviderRegistry>,
    scheduler: Option<SchedulerHandle>,
    watchers: Arc<WatcherRegistry>,
}

impl Server {
    pub fn new(
        socket_path: PathBuf,
        cache: Arc<Cache>,
        registry: Arc<ProviderRegistry>,
        scheduler: Option<SchedulerHandle>,
        watchers: Arc<WatcherRegistry>,
    ) -> Self {
        Self {
            socket_path,
            cache,
            registry,
            scheduler,
            watchers,
        }
    }

    pub async fn run(&self) -> std::io::Result<()> {
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Clean up stale socket file. If another daemon is actively listening,
        // the bind will fail with EADDRINUSE — that's correct behavior.
        if self.socket_path.exists() {
            // Check if something is actually listening
            if std::os::unix::net::UnixStream::connect(&self.socket_path).is_ok() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!(
                        "Another daemon is already listening on {:?}",
                        self.socket_path
                    ),
                ));
            }
            // Stale socket file — remove it
            let _ = std::fs::remove_file(&self.socket_path);
        }

        let listener = UnixListener::bind(&self.socket_path)?;
        info!("Listening on {:?}", self.socket_path);

        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let cache = Arc::clone(&self.cache);
                    let registry = Arc::clone(&self.registry);
                    let scheduler = self.scheduler.clone();
                    let watchers = self.watchers.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            handle_connection(stream, cache, registry, scheduler, watchers).await
                        {
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
    watchers: Arc<WatcherRegistry>,
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

        match serde_json::from_str::<Request>(trimmed) {
            Ok(Request::Watch { key, path, format }) => {
                // Watch takes over the connection — enter streaming mode
                handle_watch(
                    key,
                    path,
                    format,
                    &context_path,
                    &cache,
                    &registry,
                    scheduler.as_ref(),
                    &watchers,
                    &mut writer,
                )
                .await;
                return Ok(());
            }
            Ok(request) => {
                let response = handle_request(
                    &request,
                    &cache,
                    &registry,
                    scheduler.as_ref(),
                    &mut context_path,
                )
                .await;
                let response_bytes = format_response(&request, &response);
                writer.write_all(response_bytes.as_bytes()).await?;
            }
            Err(e) => {
                let resp = Response::error(format!("invalid request: {e}"));
                let mut out = serde_json::to_string(&resp).unwrap();
                out.push('\n');
                writer.write_all(out.as_bytes()).await?;
            }
        };
        line.clear();
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_watch(
    key: String,
    path: Option<String>,
    format: Format,
    context_path: &Option<String>,
    cache: &Cache,
    registry: &ProviderRegistry,
    scheduler: Option<&SchedulerHandle>,
    watchers: &WatcherRegistry,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
) {
    let (provider_name, field) = protocol::split_key(&key);

    let effective_path = resolve_path(path.as_deref(), context_path, provider_name, registry);

    // Signal demand
    if let Some(sched) = scheduler {
        sched
            .send(SchedulerMessage::QueryActivity {
                provider: provider_name.to_string(),
                path: effective_path.clone(),
            })
            .await;
    }

    // Subscribe to notifications
    let mut rx = watchers.subscribe(provider_name, effective_path.as_deref());

    // Send initial value
    let initial = read_watch_value(cache, provider_name, field, effective_path.as_deref());
    if write_watch_line(writer, &initial, &format).await.is_err() {
        return;
    }

    let mut last_data = initial.data.clone();

    // Stream loop
    loop {
        match rx.recv().await {
            Ok(()) => {
                // Signal ongoing demand
                if let Some(sched) = scheduler {
                    sched
                        .send(SchedulerMessage::QueryActivity {
                            provider: provider_name.to_string(),
                            path: effective_path.clone(),
                        })
                        .await;
                }

                let response =
                    read_watch_value(cache, provider_name, field, effective_path.as_deref());

                // Field-level filtering: skip if value unchanged
                if response.data == last_data {
                    continue;
                }
                last_data = response.data.clone();

                if write_watch_line(writer, &response, &format).await.is_err() {
                    break; // Client disconnected
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                debug!("Watch subscriber lagged by {n} messages, catching up");
                let response =
                    read_watch_value(cache, provider_name, field, effective_path.as_deref());
                if response.data != last_data {
                    last_data = response.data.clone();
                    if write_watch_line(writer, &response, &format).await.is_err() {
                        break;
                    }
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                break;
            }
        }
    }
}

fn read_watch_value(
    cache: &Cache,
    provider_name: &str,
    field: Option<&str>,
    path: Option<&str>,
) -> Response {
    match cache.get(provider_name, path) {
        Some(entry) => {
            let age_ms = entry.age_ms();
            let stale = entry.is_stale();
            let data = if let Some(field_name) = field {
                match entry.result.get(field_name) {
                    Some(value) => serde_json::to_value(value).unwrap(),
                    None => {
                        return Response::error(format!(
                            "unknown field: {provider_name}.{field_name}"
                        ));
                    }
                }
            } else {
                entry.result.to_json()
            };
            Response::ok(data, age_ms, stale)
        }
        None => Response::miss(),
    }
}

async fn write_watch_line(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    response: &Response,
    format: &Format,
) -> Result<(), std::io::Error> {
    let line = format_data(format, response);
    writer.write_all(line.as_bytes()).await
}

/// Resolve the effective path for a request: explicit path > context path > None.
/// Only applies the context for path-scoped (non-global) providers.
/// Relative paths are canonicalized to absolute paths.
fn resolve_path(
    explicit: Option<&str>,
    context: &Option<String>,
    provider_name: &str,
    registry: &ProviderRegistry,
) -> Option<String> {
    let raw = if explicit.is_some() {
        explicit.map(|s| s.to_string())
    } else if let Some(provider) = registry.get(provider_name) {
        if !provider.metadata().global {
            context.clone()
        } else {
            None
        }
    } else {
        None
    };

    // Canonicalize relative paths to absolute
    raw.map(|p| {
        let path = std::path::Path::new(&p);
        if path.is_relative() {
            std::env::current_dir()
                .ok()
                .and_then(|cwd| cwd.join(path).canonicalize().ok())
                .map(|abs| abs.to_string_lossy().to_string())
                .unwrap_or(p)
        } else {
            // Canonicalize absolute paths too (resolve symlinks, ..)
            path.canonicalize()
                .map(|abs| abs.to_string_lossy().to_string())
                .unwrap_or(p)
        }
    })
}

async fn handle_request(
    request: &Request,
    cache: &Cache,
    registry: &ProviderRegistry,
    scheduler: Option<&SchedulerHandle>,
    context_path: &mut Option<String>,
) -> Response {
    match request {
        Request::Get { key, path, .. } => {
            let (provider_name, field) = protocol::split_key(key);

            if registry.get_source(provider_name).is_none() {
                return Response::error(format!("unknown provider: {provider_name}"));
            }

            let effective_path =
                resolve_path(path.as_deref(), context_path, provider_name, registry);

            // Signal demand to scheduler — this keeps the data warm.
            if let Some(sched) = scheduler {
                sched
                    .send(SchedulerMessage::QueryActivity {
                        provider: provider_name.to_string(),
                        path: effective_path.clone(),
                    })
                    .await;
            }

            match cache.get(provider_name, effective_path.as_deref()) {
                Some(entry) => {
                    let age_ms = entry.age_ms();
                    let stale = entry.is_stale();

                    let data = if let Some(field_name) = field {
                        match entry.result.get(field_name) {
                            Some(value) => serde_json::to_value(value).unwrap(),
                            None => {
                                return Response::error(format!(
                                    "unknown field: {provider_name}.{field_name}"
                                ));
                            }
                        }
                    } else {
                        entry.result.to_json()
                    };
                    Response::ok(data, age_ms, stale)
                }
                None => {
                    // Synchronous cache miss: execute the provider inline if possible.
                    let provider = registry.get(provider_name);
                    match provider {
                        Some(provider) => {
                            let path_owned = effective_path.clone();
                            let result = tokio::task::spawn_blocking(move || {
                                provider.execute(path_owned.as_deref())
                            })
                            .await
                            .ok()
                            .flatten();

                            match result {
                                Some(result) => {
                                    cache.put(
                                        provider_name,
                                        effective_path.as_deref(),
                                        result.clone(),
                                    );
                                    let data = if let Some(field_name) = field {
                                        match result.get(field_name) {
                                            Some(value) => serde_json::to_value(value).unwrap(),
                                            None => {
                                                return Response::error(format!(
                                                    "unknown field: {provider_name}.{field_name}"
                                                ));
                                            }
                                        }
                                    } else {
                                        result.to_json()
                                    };
                                    Response::ok(data, 0, false)
                                }
                                None => Response::miss(),
                            }
                        }
                        // Virtual provider or provider with no execute — return miss
                        None => Response::miss(),
                    }
                }
            }
        }
        Request::Poke { key, path } => {
            let (provider_name, _) = protocol::split_key(key);
            let effective_path =
                resolve_path(path.as_deref(), context_path, provider_name, registry);

            if let Some(sched) = scheduler {
                // Route through scheduler.
                sched
                    .send(SchedulerMessage::Poke {
                        provider: provider_name.to_string(),
                        path: effective_path,
                    })
                    .await;
                Response {
                    ok: true,
                    data: None,
                    age_ms: None,
                    stale: None,
                    error: None,
                }
            } else {
                // Fallback: execute directly (used by tests with None scheduler).
                // Check if it's a virtual provider — poke is a no-op
                if registry.get_source(provider_name)
                    == Some(crate::provider::ProviderSource::Virtual)
                {
                    return Response {
                        ok: true,
                        data: None,
                        age_ms: None,
                        stale: None,
                        error: None,
                    };
                }
                match registry.get(provider_name) {
                    Some(provider) => {
                        if let Some(result) = provider.execute(effective_path.as_deref()) {
                            cache.put(provider_name, effective_path.as_deref(), result);
                        }
                        Response {
                            ok: true,
                            data: None,
                            age_ms: None,
                            stale: None,
                            error: None,
                        }
                    }
                    None => Response::error(format!("unknown provider: {provider_name}")),
                }
            }
        }
        Request::Context { path } => {
            *context_path = Some(path.clone());
            Response {
                ok: true,
                data: None,
                age_ms: None,
                stale: None,
                error: None,
            }
        }
        Request::List => {
            let providers: Vec<serde_json::Value> = registry
                .list()
                .into_iter()
                .map(|name| {
                    if let Some(provider) = registry.get(&name) {
                        let meta = provider.metadata();
                        serde_json::json!({
                            "name": name,
                            "source": registry.get_source(&name),
                            "global": meta.global,
                            "fields": meta.fields.iter().map(|f| &f.name).collect::<Vec<_>>(),
                        })
                    } else {
                        // Virtual provider — no backing Provider trait object.
                        serde_json::json!({
                            "name": name,
                            "source": registry.get_source(&name),
                            "global": true,
                            "fields": serde_json::Value::Array(vec![]),
                        })
                    }
                })
                .collect();
            Response::ok(serde_json::json!(providers), 0, false)
        }
        Request::Store {
            key,
            data,
            ttl,
            path,
        } => {
            // Reject if a builtin or script provider already owns this name.
            if registry.has_non_virtual(key) {
                return Response::error(format!(
                    "cannot store under '{key}': name is used by a builtin or script provider"
                ));
            }

            // data must be a JSON object; its top-level keys become fields.
            let obj = match data.as_object() {
                Some(o) => o,
                None => return Response::error("store data must be a JSON object"),
            };

            // Convert JSON object fields to ProviderResult.
            let mut result = crate::provider::ProviderResult::new();
            for (field_key, field_val) in obj {
                let value = match field_val {
                    serde_json::Value::String(s) => crate::provider::Value::String(s.clone()),
                    serde_json::Value::Bool(b) => crate::provider::Value::Bool(*b),
                    serde_json::Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            crate::provider::Value::Int(i)
                        } else if let Some(f) = n.as_f64() {
                            crate::provider::Value::Float(f)
                        } else {
                            crate::provider::Value::String(n.to_string())
                        }
                    }
                    other => crate::provider::Value::String(other.to_string()),
                };
                result.insert(field_key.clone(), value);
            }

            // Parse optional TTL.
            let interval_secs = ttl
                .as_deref()
                .and_then(crate::scheduler::parse_duration_secs_pub);

            // Resolve optional path — canonicalize if provided.
            let effective_path: Option<String> = path.as_deref().map(|p| {
                let path_obj = std::path::Path::new(p);
                if path_obj.is_relative() {
                    std::env::current_dir()
                        .ok()
                        .and_then(|cwd| cwd.join(path_obj).canonicalize().ok())
                        .map(|abs| abs.to_string_lossy().to_string())
                        .unwrap_or_else(|| p.to_string())
                } else {
                    path_obj
                        .canonicalize()
                        .map(|abs| abs.to_string_lossy().to_string())
                        .unwrap_or_else(|_| p.to_string())
                }
            });

            // Register virtual name (idempotent, safe under concurrent access).
            registry.register_virtual(key);

            // Write to cache.
            cache.put_with_interval(key, effective_path.as_deref(), result, interval_secs);

            Response {
                ok: true,
                data: None,
                age_ms: None,
                stale: None,
                error: None,
            }
        }
        Request::Status => {
            let cache_details = cache.list_entries();
            let mut status_data = serde_json::json!({
                "pid": std::process::id(),
                "version": env!("CARGO_PKG_VERSION"),
                "cache_entries": cache.len(),
                "cache": serde_json::to_value(&cache_details).unwrap_or_default(),
                "providers": registry.list().len(),
            });

            // Get scheduler status if available.
            if let Some(sched) = scheduler
                && let Some(sched_status) = sched.get_status().await
            {
                status_data["watched_paths"] =
                    serde_json::to_value(&sched_status.watched_paths).unwrap_or_default();
                status_data["in_flight"] =
                    serde_json::to_value(&sched_status.in_flight).unwrap_or_default();
                status_data["backoff"] =
                    serde_json::to_value(&sched_status.backoff).unwrap_or_default();
                status_data["poll_timers"] =
                    serde_json::to_value(&sched_status.poll_timers).unwrap_or_default();
                status_data["demand"] =
                    serde_json::to_value(&sched_status.demand).unwrap_or_default();
            }

            Response::ok(status_data, 0, false)
        }
        // Watch is intercepted in handle_connection before reaching here
        Request::Watch { .. } => unreachable!("Watch handled before handle_request"),
    }
}

fn format_response(request: &Request, response: &Response) -> String {
    let format = match request {
        Request::Get { format, .. } => format,
        _ => &Format::Json,
    };

    format_data(format, response)
}

fn format_data(format: &Format, response: &Response) -> String {
    match format {
        Format::Text => {
            if !response.ok {
                return format!(
                    "error: {}\n",
                    response.error.as_deref().unwrap_or("unknown")
                );
            }
            match &response.data {
                Some(serde_json::Value::String(s)) => format!("{s}\n\n"),
                Some(serde_json::Value::Number(n)) => format!("{n}\n\n"),
                Some(serde_json::Value::Bool(b)) => format!("{b}\n\n"),
                Some(serde_json::Value::Object(map)) => {
                    let mut pairs: Vec<(&String, &serde_json::Value)> = map.iter().collect();
                    pairs.sort_by_key(|(k, _)| *k);
                    let vals: Vec<String> = pairs
                        .iter()
                        .map(|(_, v)| match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect();
                    let mut out = vals.join("\n");
                    out.push_str("\n\n");
                    out
                }
                Some(serde_json::Value::Null) | None => "\n".to_string(),
                Some(other) => format!("{other}\n\n"),
            }
        }
        Format::Sh => {
            if !response.ok {
                return format!(
                    "error: {}\n",
                    response.error.as_deref().unwrap_or("unknown")
                );
            }
            match &response.data {
                Some(serde_json::Value::String(s)) => format!("{s}\n\n"),
                Some(serde_json::Value::Number(n)) => format!("{n}\n\n"),
                Some(serde_json::Value::Bool(b)) => format!("{b}\n\n"),
                Some(serde_json::Value::Object(map)) => {
                    let mut lines: Vec<String> = map
                        .iter()
                        .map(|(k, v)| {
                            let val = match v {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            format!("{k}={val}")
                        })
                        .collect();
                    lines.sort();
                    let mut out = lines.join("\n");
                    out.push_str("\n\n");
                    out
                }
                Some(serde_json::Value::Null) | None => "\n".to_string(),
                Some(other) => format!("{other}\n\n"),
            }
        }
        Format::Json => {
            let mut out = serde_json::to_string(response).unwrap();
            out.push('\n');
            out
        }
    }
}
