use crate::cache::Cache;
use crate::config::Config;
use crate::provider::InvalidationStrategy;
use crate::protocol::{self, Format, IntrospectSubject, Request, Response};
use crate::provider::registry::ProviderRegistry;
use crate::provider::ProviderSource;
use crate::scheduler::{BackoffInfo, DemandInfo, PollTimerInfo, SchedulerHandle, SchedulerMessage};
use crate::watcher_registry::WatcherRegistry;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{debug, info, warn};

pub struct Server {
    socket_path: PathBuf,
    cache: Arc<Cache>,
    registry: Arc<ProviderRegistry>,
    scheduler: Option<SchedulerHandle>,
    watchers: Arc<WatcherRegistry>,
    start_instant: Instant,
    requests_total: Arc<AtomicU64>,
    config: Arc<Config>,
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
            start_instant: Instant::now(),
            requests_total: Arc::new(AtomicU64::new(0)),
            config: Arc::new(Config::load()),
        }
    }

    pub fn new_with_config(
        socket_path: PathBuf,
        cache: Arc<Cache>,
        registry: Arc<ProviderRegistry>,
        scheduler: Option<SchedulerHandle>,
        watchers: Arc<WatcherRegistry>,
        config: Config,
    ) -> Self {
        Self {
            socket_path,
            cache,
            registry,
            scheduler,
            watchers,
            start_instant: Instant::now(),
            requests_total: Arc::new(AtomicU64::new(0)),
            config: Arc::new(config),
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
                    let start_instant = self.start_instant;
                    let requests_total = Arc::clone(&self.requests_total);
                    let socket_path = self.socket_path.clone();
                    let config = Arc::clone(&self.config);
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(
                            stream,
                            cache,
                            registry,
                            scheduler,
                            watchers,
                            start_instant,
                            requests_total,
                            socket_path,
                            config,
                        )
                        .await
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

#[allow(clippy::too_many_arguments)]
async fn handle_connection(
    stream: tokio::net::UnixStream,
    cache: Arc<Cache>,
    registry: Arc<ProviderRegistry>,
    scheduler: Option<SchedulerHandle>,
    watchers: Arc<WatcherRegistry>,
    start_instant: Instant,
    requests_total: Arc<AtomicU64>,
    socket_path: PathBuf,
    config: Arc<Config>,
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
                requests_total.fetch_add(1, Ordering::Relaxed);
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
                requests_total.fetch_add(1, Ordering::Relaxed);
                let response = handle_request(
                    &request,
                    &cache,
                    &registry,
                    scheduler.as_ref(),
                    &mut context_path,
                    start_instant,
                    &watchers,
                    &requests_total,
                    &socket_path,
                    &config,
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
    watchers: &Arc<WatcherRegistry>,
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

/// The set of metadata suffix names the server recognises. Keep this in sync
/// with the `match meta` post-processing arms in `handle_request`.
const KNOWN_METADATA_SUFFIXES: &[&str] = &["age", "stale", "fresh", "cache", "source"];

/// Parse a metadata suffix from a key. "git.branch:fresh" → ("git.branch", Some("fresh")).
/// Only suffixes listed in KNOWN_METADATA_SUFFIXES are stripped; anything else passes
/// through as part of the key. Metadata suffix splitting must run BEFORE
/// protocol::split_key so the field/suffix disambiguation is correct.
fn split_metadata_suffix(key: &str) -> (&str, Option<&str>) {
    if let Some((base, meta)) = key.rsplit_once(':')
        && KNOWN_METADATA_SUFFIXES.contains(&meta)
    {
        return (base, Some(meta));
    }
    (key, None)
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

#[allow(clippy::too_many_arguments)]
async fn handle_request(
    request: &Request,
    cache: &Cache,
    registry: &ProviderRegistry,
    scheduler: Option<&SchedulerHandle>,
    context_path: &mut Option<String>,
    start_instant: Instant,
    watchers: &WatcherRegistry,
    requests_total: &AtomicU64,
    socket_path: &std::path::Path,
    config: &Config,
) -> Response {
    match request {
        Request::Get {
            key,
            path,
            force,
            wait,
            ..
        } => {
            let (stripped_key, meta) = split_metadata_suffix(key);
            let (provider_name, field) = protocol::split_key(stripped_key);

            if registry.get_source(provider_name).is_none() {
                return Response::error(format!("unknown provider: {provider_name}"));
            }

            let effective_path =
                resolve_path(path.as_deref(), context_path, provider_name, registry);

            // Force evict: drop the cache entry so the normal miss path re-executes.
            // force wins over wait — if force triggered eviction (or a virtual-provider
            // error), we never reach the wait block below.
            if *force {
                if registry.get_source(provider_name)
                    == Some(crate::provider::ProviderSource::Virtual)
                {
                    return Response::error(format!(
                        "cannot --force virtual provider '{provider_name}': no source to re-execute from"
                    ));
                }
                cache.remove(provider_name, effective_path.as_deref());
            }

            // Wait semantics: if the cached entry is stale, evict it so the normal
            // miss path below re-executes the provider inline and returns fresh data.
            // Skipped for virtual providers — they have no source to re-execute; the
            // cached value is all there is, and we return it regardless of staleness.
            // force wins over wait — force is handled above; !force guards this block.
            if *wait
                && !*force
                && registry.get_source(provider_name)
                    != Some(crate::provider::ProviderSource::Virtual)
                && cache
                    .get(provider_name, effective_path.as_deref())
                    .is_some_and(|e| e.is_stale())
            {
                cache.remove(provider_name, effective_path.as_deref());
                // Fall through to the normal miss path, which executes inline.
            }

            // Signal demand to scheduler — this keeps the data warm.
            if let Some(sched) = scheduler {
                sched
                    .send(SchedulerMessage::QueryActivity {
                        provider: provider_name.to_string(),
                        path: effective_path.clone(),
                    })
                    .await;
            }

            // :source short-circuits — no cache lookup needed.
            if matches!(meta, Some("source")) {
                let src = registry
                    .get_source(provider_name)
                    .map(|s| match s {
                        crate::provider::ProviderSource::Builtin => "builtin",
                        crate::provider::ProviderSource::Script => "script",
                        crate::provider::ProviderSource::Virtual => "virtual",
                    })
                    .unwrap_or("unknown");
                return Response::ok(serde_json::Value::String(src.to_string()), 0, false);
            }

            let (cache_hit, normal_response) = match cache
                .get(provider_name, effective_path.as_deref())
            {
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
                    (true, Response::ok(data, age_ms, stale))
                }
                None => {
                    // Synchronous cache miss: execute the provider inline if possible.
                    let provider = registry.get(provider_name);
                    match provider {
                        Some(provider) => {
                            let interval = crate::provider::expected_interval_secs(
                                &provider.metadata().invalidation,
                            );
                            let path_owned = effective_path.clone();
                            let result = tokio::task::spawn_blocking(move || {
                                provider.execute(path_owned.as_deref())
                            })
                            .await
                            .ok()
                            .flatten();

                            match result {
                                Some(result) => {
                                    cache.put_with_interval(
                                        provider_name,
                                        effective_path.as_deref(),
                                        result.clone(),
                                        interval,
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
                                    (false, Response::ok(data, 0, false))
                                }
                                None => (false, Response::miss()),
                            }
                        }
                        // Virtual provider or provider with no execute — return miss
                        None => (false, Response::miss()),
                    }
                }
            };

            match meta {
                None => normal_response,
                Some("age") => Response::ok(
                    normal_response
                        .age_ms
                        .map(|n| {
                            // age_ms is u128 but realistic daemon ages fit in u64 (~585M years).
                            serde_json::Value::Number(serde_json::Number::from(n as u64))
                        })
                        .unwrap_or(serde_json::Value::Null),
                    0,
                    false,
                ),
                Some("stale") => Response::ok(
                    normal_response
                        .stale
                        .map(serde_json::Value::Bool)
                        .unwrap_or(serde_json::Value::Null),
                    0,
                    false,
                ),
                Some("fresh") => Response::ok(
                    normal_response
                        .stale
                        .map(|s| serde_json::Value::Bool(!s))
                        .unwrap_or(serde_json::Value::Null),
                    0,
                    false,
                ),
                Some("cache") => Response::ok(serde_json::Value::Bool(cache_hit), 0, false),
                Some(other) => Response::error(format!("unknown metadata suffix: :{other}")),
            }
        }
        Request::Refresh { key, path } => {
            let (provider_name, _) = protocol::split_key(key);
            let effective_path =
                resolve_path(path.as_deref(), context_path, provider_name, registry);

            if let Some(sched) = scheduler {
                // Route through scheduler.
                sched
                    .send(SchedulerMessage::Refresh {
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
                // Check if it's a virtual provider — refresh is a no-op
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
                        let interval = crate::provider::expected_interval_secs(
                            &provider.metadata().invalidation,
                        );
                        if let Some(result) = provider.execute(effective_path.as_deref()) {
                            cache.put_with_interval(
                                provider_name,
                                effective_path.as_deref(),
                                result,
                                interval,
                            );
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
        Request::Put {
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

            // data=None means "clear the cache entry" — remove the row but keep the registry.
            let Some(data) = data else {
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
                cache.remove(key, effective_path.as_deref());
                return Response {
                    ok: true,
                    data: None,
                    age_ms: None,
                    stale: None,
                    error: None,
                };
            };

            // data must be a JSON object; its top-level keys become fields.
            let obj = match data.as_object() {
                Some(o) => o,
                None => return Response::error("put data must be a JSON object"),
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
                "uptime_secs": start_instant.elapsed().as_secs(),
                "active_watchers": watchers.entry_count() as u64,
                "requests_total": requests_total.load(Ordering::Relaxed),
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
        Request::Introspect {
            subject,
            duration_secs: _,
        } => match subject {
            IntrospectSubject::Daemon => {
                // Gather in_flight from scheduler status if available.
                let in_flight_count = if let Some(sched) = scheduler
                    && let Some(sched_status) = sched.get_status().await
                {
                    sched_status.in_flight.len() as u64
                } else {
                    0
                };
                let active_watchers = watchers.entry_count() as u64;
                let cache_entries = cache.len() as u64;
                handle_introspect_daemon(
                    socket_path,
                    start_instant,
                    requests_total,
                    in_flight_count,
                    active_watchers,
                    cache_entries,
                )
            }
            IntrospectSubject::Providers => {
                handle_introspect_providers(registry, scheduler).await
            }
            IntrospectSubject::Config => {
                handle_introspect_config(config)
            }
            IntrospectSubject::Cache => {
                handle_introspect_cache(cache)
            }
            IntrospectSubject::Backoff => {
                handle_introspect_backoff(scheduler).await
            }
            IntrospectSubject::Watches => {
                handle_introspect_watches(scheduler).await
            }
            IntrospectSubject::Timers => {
                handle_introspect_timers(scheduler).await
            }
            IntrospectSubject::Demand => {
                handle_introspect_demand(scheduler).await
            }
            _ => Response::error(format!(
                "introspect subject '{:?}' not yet implemented",
                subject
            )),
        }
        // Watch is intercepted in handle_connection before reaching here
        Request::Watch { .. } => unreachable!("Watch handled before handle_request"),
    }
}

fn handle_introspect_daemon(
    socket_path: &std::path::Path,
    start_instant: Instant,
    requests_total: &AtomicU64,
    in_flight_count: u64,
    active_watchers: u64,
    cache_entries: u64,
) -> Response {
    let config_path = crate::config::Config::config_path_if_exists()
        .map(|p| serde_json::Value::String(p.to_string_lossy().into_owned()))
        .unwrap_or(serde_json::Value::Null);

    let uptime_secs = start_instant.elapsed().as_secs();

    let mut verdicts = vec![
        serde_json::json!({"level": "PASS", "message": "daemon responsive"}),
    ];

    if in_flight_count > 50 {
        verdicts.push(serde_json::json!({
            "level": "WARN",
            "message": format!("{in_flight_count} in-flight requests (threshold 50)")
        }));
    } else {
        verdicts.push(serde_json::json!({
            "level": "PASS",
            "message": format!("in_flight={in_flight_count}")
        }));
    }

    let data = serde_json::json!({
        "pid": std::process::id(),
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": uptime_secs,
        "socket_path": socket_path.to_string_lossy().as_ref(),
        "config_path": config_path,
        "requests_total": requests_total.load(Ordering::Relaxed),
        "in_flight": in_flight_count,
        "active_watchers": active_watchers,
        "cache_entries": cache_entries,
        "verdicts": verdicts,
    });

    Response::ok(data, 0, false)
}

fn summarize_invalidation(strategy: &InvalidationStrategy) -> String {
    match strategy {
        InvalidationStrategy::Once => "once".to_string(),
        InvalidationStrategy::Poll { interval_secs, .. } => format!("poll {interval_secs}s"),
        InvalidationStrategy::Watch {
            patterns,
            fallback_poll_secs,
        } => {
            let pats = patterns.join(",");
            match fallback_poll_secs {
                Some(s) => format!("watch {pats} + poll {s}s"),
                None => format!("watch {pats}"),
            }
        }
        InvalidationStrategy::WatchAndPoll {
            patterns,
            interval_secs,
            ..
        } => {
            let pats = patterns.join(",");
            format!("watch {pats} + poll {interval_secs}s")
        }
    }
}

async fn handle_introspect_providers(
    registry: &ProviderRegistry,
    scheduler: Option<&SchedulerHandle>,
) -> Response {
    let backoff_list: Vec<BackoffInfo> = if let Some(s) = scheduler {
        s.get_status()
            .await
            .map(|st| st.backoff)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut providers_out = Vec::new();
    let mut verdicts = Vec::new();

    let mut names = registry.list();
    names.sort();

    for name in &names {
        let source = registry
            .get_source(name)
            .map(|s| match s {
                ProviderSource::Builtin => "builtin",
                ProviderSource::Script => "script",
                ProviderSource::Virtual => "virtual",
            })
            .unwrap_or("unknown");

        let (scope, fields, invalidation) = if let Some(p) = registry.get(name) {
            let meta = p.metadata();
            let scope = if meta.global { "global" } else { "path" };
            let fields: Vec<serde_json::Value> = meta
                .fields
                .iter()
                .map(|f| {
                    let type_str = match f.field_type {
                        crate::provider::FieldType::String => "string",
                        crate::provider::FieldType::Int => "int",
                        crate::provider::FieldType::Bool => "bool",
                        crate::provider::FieldType::Float => "float",
                        crate::provider::FieldType::Object => "object",
                    };
                    serde_json::json!({
                        "name": f.name,
                        "type": type_str,
                    })
                })
                .collect();
            let invalidation = summarize_invalidation(&meta.invalidation);
            (scope, fields, invalidation)
        } else {
            ("global", Vec::new(), "data-only".to_string())
        };

        let relevant: Vec<&BackoffInfo> = backoff_list.iter().filter(|b| &b.provider == name).collect();
        let in_backoff = if relevant.is_empty() {
            serde_json::Value::Null
        } else {
            let worst = relevant.iter().max_by_key(|b| b.elapsed_secs).unwrap();
            serde_json::json!({
                "stage": worst.stage,
                "elapsed_secs": worst.elapsed_secs,
            })
        };

        if !relevant.is_empty() {
            let worst = relevant.iter().max_by_key(|b| b.elapsed_secs).unwrap();
            verdicts.push(serde_json::json!({
                "level": "WARN",
                "message": format!("{name} in backoff {}s (stage={})", worst.elapsed_secs, worst.stage)
            }));
        }

        providers_out.push(serde_json::json!({
            "name": name,
            "source": source,
            "scope": scope,
            "fields": fields,
            "invalidation": invalidation,
            "in_backoff": in_backoff,
        }));
    }

    verdicts.insert(
        0,
        serde_json::json!({
            "level": "PASS",
            "message": format!("{} providers registered", names.len()),
        }),
    );

    Response::ok(
        serde_json::json!({
            "providers": providers_out,
            "verdicts": verdicts,
        }),
        0,
        false,
    )
}

fn handle_introspect_config(config: &Config) -> Response {
    let path = Config::config_path_if_exists();
    let path_json = match path {
        Some(ref p) => serde_json::Value::String(p.display().to_string()),
        None => serde_json::Value::Null,
    };
    let provider_count = config.providers.len() as u64;
    let verdicts = vec![
        serde_json::json!({"level": "PASS", "message": "config parsed"}),
        serde_json::json!({"level": "PASS", "message": format!("{provider_count} provider definitions loaded")}),
    ];
    Response::ok(
        serde_json::json!({
            "path": path_json,
            "parsed": true,
            "errors": [],
            "provider_count_from_config": provider_count,
            "verdicts": verdicts,
        }),
        0,
        false,
    )
}

fn handle_introspect_cache(cache: &Cache) -> Response {
    let entries = cache.list_entries();
    let total = entries.len() as u64;
    let stale = entries.iter().filter(|e| e.stale).count() as u64;
    let ratio = if total == 0 {
        0.0_f64
    } else {
        stale as f64 / total as f64
    };

    let mut verdicts = vec![serde_json::json!({
        "level": "PASS",
        "message": format!("{total} entries"),
    })];
    if stale > 0 {
        verdicts.push(serde_json::json!({
            "level": "WARN",
            "message": format!("{stale} stale — run `comb status --filter stale=true` to inspect"),
        }));
    } else if total > 0 {
        verdicts.push(serde_json::json!({"level": "PASS", "message": "no stale entries"}));
    }

    Response::ok(
        serde_json::json!({
            "total_entries": total,
            "stale_entries": stale,
            "stale_ratio": ratio,
            "verdicts": verdicts,
        }),
        0,
        false,
    )
}

async fn handle_introspect_backoff(scheduler: Option<&SchedulerHandle>) -> Response {
    let backoff: Vec<BackoffInfo> = if let Some(s) = scheduler {
        s.get_status().await.map(|st| st.backoff).unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut verdicts = Vec::new();
    if backoff.is_empty() {
        verdicts.push(serde_json::json!({"level": "PASS", "message": "no providers in backoff"}));
    } else {
        for entry in &backoff {
            let label = match &entry.path {
                Some(p) => format!("{} ({p})", entry.provider),
                None => entry.provider.clone(),
            };
            verdicts.push(serde_json::json!({
                "level": "WARN",
                "message": format!("{label} — stage={} elapsed={}s", entry.stage, entry.elapsed_secs),
            }));
        }
    }

    let backoff_json: Vec<serde_json::Value> = backoff
        .iter()
        .map(|b| serde_json::to_value(b).unwrap_or(serde_json::Value::Null))
        .collect();

    Response::ok(
        serde_json::json!({
            "backoff": backoff_json,
            "verdicts": verdicts,
        }),
        0,
        false,
    )
}

async fn handle_introspect_watches(scheduler: Option<&SchedulerHandle>) -> Response {
    let paths: Vec<String> = if let Some(s) = scheduler {
        s.get_status()
            .await
            .map(|st| st.watched_paths)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let verdict = if paths.is_empty() {
        serde_json::json!({"level": "WARN", "message": "not watching any paths"})
    } else {
        serde_json::json!({"level": "PASS", "message": format!("watching {} paths", paths.len())})
    };

    Response::ok(
        serde_json::json!({
            "paths": paths,
            "verdicts": [verdict],
        }),
        0,
        false,
    )
}

async fn handle_introspect_timers(scheduler: Option<&SchedulerHandle>) -> Response {
    let timers: Vec<PollTimerInfo> = if let Some(s) = scheduler {
        s.get_status()
            .await
            .map(|st| st.poll_timers)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut verdicts = vec![serde_json::json!({
        "level": "PASS",
        "message": format!("{} poll timers", timers.len()),
    })];

    for t in &timers {
        if t.interval_secs > 0 && t.last_run_secs_ago > t.interval_secs * 2 {
            let label = match &t.path {
                Some(p) => format!("{} ({p})", t.provider),
                None => t.provider.clone(),
            };
            verdicts.push(serde_json::json!({
                "level": "WARN",
                "message": format!("{label} overdue (interval={}s, last={}s ago)", t.interval_secs, t.last_run_secs_ago),
            }));
        }
    }

    let timers_json: Vec<serde_json::Value> = timers
        .iter()
        .map(|t| serde_json::to_value(t).unwrap_or(serde_json::Value::Null))
        .collect();

    Response::ok(
        serde_json::json!({
            "timers": timers_json,
            "verdicts": verdicts,
        }),
        0,
        false,
    )
}

async fn handle_introspect_demand(scheduler: Option<&SchedulerHandle>) -> Response {
    let demand: Vec<DemandInfo> = if let Some(s) = scheduler {
        s.get_status()
            .await
            .map(|st| st.demand)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let verdict = serde_json::json!({
        "level": "PASS",
        "message": format!("{} active keys", demand.len()),
    });

    let demand_json: Vec<serde_json::Value> = demand
        .iter()
        .map(|d| serde_json::to_value(d).unwrap_or(serde_json::Value::Null))
        .collect();

    Response::ok(
        serde_json::json!({
            "demand": demand_json,
            "verdicts": [verdict],
        }),
        0,
        false,
    )
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
                    "error: {}\n\n",
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
                        .flat_map(|(k, v)| {
                            if let serde_json::Value::Object(inner) = v {
                                // Nested object: flatten as outer.inner=value
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
                                vec![val]
                            }
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
        Format::Sh => {
            if !response.ok {
                return format!(
                    "error: {}\n\n",
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
                        .flat_map(|(k, v)| {
                            if let serde_json::Value::Object(inner) = v {
                                // Nested object: flatten as outer.inner=value
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
