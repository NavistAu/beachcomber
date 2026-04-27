use crate::cache::Cache;
use crate::config::Config;
use crate::protocol::{self, Format, IntrospectSubject, Request, Response};
use crate::provider::registry::ProviderRegistry;
use crate::provider::{InvalidationStrategy, SourceScope};
use crate::scheduler::{
    DemandInfo, LifecycleInfo, PollTimerInfo, SchedulerHandle, SchedulerMessage,
};
use crate::watcher_registry::WatcherRegistry;
use std::collections::HashMap;
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

/// Parse a request key into one of four forms, disambiguating source vs field
/// for 2-segment keys using the registry.
pub enum KeyParse {
    /// "provider"
    Provider(String),
    /// "provider.field" — x is a field name (not a registered source)
    Field(String, String),
    /// "provider.source" — x is a registered source name
    Source(String, String),
    /// "provider.source.field"
    SourceField(String, String, String),
}

/// Disambiguates between Field and Source forms for 2-segment keys.
/// Source name takes precedence over field name when both could match.
/// For 3-part keys `p.s.f`: if `s` is a registered source name for `p`, this
/// is a SourceField lookup; otherwise `p.s` is a field whose value is an Object
/// and `f` is a key within that object (nested field path).
pub fn parse_key(key: &str, registry: &ProviderRegistry) -> KeyParse {
    let parts: Vec<&str> = key.split('.').collect();
    match parts.as_slice() {
        [p] => KeyParse::Provider(p.to_string()),
        [p, x] => {
            if registry.source(p, x).is_some() {
                KeyParse::Source(p.to_string(), x.to_string())
            } else {
                KeyParse::Field(p.to_string(), x.to_string())
            }
        }
        [p, s, f] => {
            if registry.source(p, s).is_some() {
                KeyParse::SourceField(p.to_string(), s.to_string(), f.to_string())
            } else {
                // s is a field name whose value is an Object; f is a sub-key.
                // Encode the nested path as "s.f" in the Field variant.
                KeyParse::Field(p.to_string(), format!("{s}.{f}"))
            }
        }
        _ => KeyParse::Provider(key.to_string()),
    }
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

    let effective_path = resolve_path(&key, path.as_deref().or(context_path.as_deref()), registry);

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

/// Cold-miss inline-execute: synchronously run a Source to populate the cache,
/// then let the caller re-read. Mirrors the scheduler's execute_source path
/// (spawn_blocking + cache.put_source) but runs in the request task so the
/// response carries fresh data (canon §"Cold cache miss triggers inline fetch").
///
/// Path argument is the consumer-supplied path; for Global sources it is
/// ignored and the cache write keys to (provider, None). Returns true if a
/// non-empty result was written.
async fn inline_execute_source(
    registry: &ProviderRegistry,
    cache: &Cache,
    provider: &str,
    source_name: &str,
    path: Option<&str>,
) -> bool {
    let Some(source) = registry.source(provider, source_name) else {
        return false;
    };
    let scope = source.metadata().scope;
    let effective_path = match scope {
        SourceScope::Global => None,
        SourceScope::PathScoped => path.map(|s| s.to_string()),
    };
    let expected_interval_secs = match source.metadata().invalidation {
        InvalidationStrategy::Poll { interval_secs } => Some(interval_secs),
        InvalidationStrategy::WatchAndPoll { interval_secs, .. } => Some(interval_secs),
        InvalidationStrategy::Watch { .. } => None,
    };
    let path_owned = effective_path.clone();
    let src_clone = std::sync::Arc::clone(&source);
    let result =
        match tokio::task::spawn_blocking(move || src_clone.execute(path_owned.as_deref())).await {
            Ok(r) => r,
            Err(_) => return false,
        };
    if result.fields.is_empty() {
        return false;
    }
    cache.put_source(
        provider,
        effective_path.as_deref(),
        source_name,
        result.fields,
        expected_interval_secs,
    );
    true
}

fn read_watch_value(
    cache: &Cache,
    provider_name: &str,
    field: Option<&str>,
    path: Option<&str>,
) -> Response {
    if let Some(field_name) = field {
        // Field-targeted: surface the owning source's last_refreshed as age,
        // not the entry-level oldest. Canon §"Field freshness".
        match cache.get_field(provider_name, path, field_name) {
            Some((value, last_refreshed)) => {
                let age_ms = last_refreshed.elapsed().as_millis();
                let data = serde_json::to_value(&value).unwrap_or(serde_json::Value::Null);
                Response::ok(data, age_ms, false)
            }
            None => match cache.get_entry(provider_name, path) {
                Some(_) => Response::error(format!("unknown field: {provider_name}.{field_name}")),
                None => Response::miss(),
            },
        }
    } else {
        match cache.get_entry(provider_name, path) {
            Some(entry) => {
                let age_ms = entry.age_ms();
                let stale = entry.is_stale();
                let flat = entry.flatten_fields();
                let data = serde_json::to_value(&flat).unwrap_or(serde_json::Value::Null);
                Response::ok(data, age_ms, stale)
            }
            None => Response::miss(),
        }
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

/// Resolve the effective path for a request. If any source at this provider is
/// PathScoped, the requested path is used (canonicalized). If all sources are
/// Global (or the provider is unknown), returns None.
///
/// For path-scoped providers, also attempts canonical_path() via the first
/// path-scoped source to walk to the project root.
fn resolve_path(
    key: &str,
    requested_path: Option<&str>,
    registry: &ProviderRegistry,
) -> Option<String> {
    let parts: Vec<&str> = key.split('.').collect();
    let provider = parts[0];

    // Virtual providers don't declare sources, but they can be stored with a path.
    // Pass the raw path through (canonicalized) so path-keyed virtual data is accessible.
    if registry.is_virtual(provider) {
        return requested_path.map(|p| {
            let path = std::path::Path::new(p);
            if path.is_relative() {
                std::env::current_dir()
                    .ok()
                    .and_then(|cwd| cwd.join(path).canonicalize().ok())
                    .map(|abs| abs.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.to_string())
            } else {
                path.canonicalize()
                    .map(|abs| abs.to_string_lossy().to_string())
                    .unwrap_or_else(|_| p.to_string())
            }
        });
    }

    let any_path_scoped = registry
        .provider_sources(provider)
        .map(|ss| ss.iter().any(|s| s.scope == SourceScope::PathScoped))
        .unwrap_or(false);

    if !any_path_scoped {
        return None;
    }

    let raw = requested_path.map(|p| {
        let path = std::path::Path::new(p);
        if path.is_relative() {
            std::env::current_dir()
                .ok()
                .and_then(|cwd| cwd.join(path).canonicalize().ok())
                .map(|abs| abs.to_string_lossy().to_string())
                .unwrap_or_else(|| p.to_string())
        } else {
            path.canonicalize()
                .map(|abs| abs.to_string_lossy().to_string())
                .unwrap_or_else(|_| p.to_string())
        }
    })?;

    // Provider-level canonicalization: find the first path-scoped source and
    // ask it to canonicalize (walk to project root, etc.).
    if let Some(sources) = registry.provider_sources(provider) {
        for sm in sources {
            if sm.scope == SourceScope::PathScoped
                && let Some(src) = registry.source(provider, &sm.name)
                && let Some(canonical) = src.canonical_path(Some(&raw))
            {
                return Some(canonical);
            }
        }
    }

    Some(raw)
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

            // Determine effective path using new per-source scope resolution.
            let requested = path.as_deref().or(context_path.as_deref());
            let effective_path = resolve_path(stripped_key, requested, registry);

            // Parse the key to understand what is being requested.
            let parsed = parse_key(stripped_key, registry);

            // Extract provider name for registry/cache lookups.
            let provider_name = match &parsed {
                KeyParse::Provider(p) => p.as_str(),
                KeyParse::Field(p, _) => p.as_str(),
                KeyParse::Source(p, _) => p.as_str(),
                KeyParse::SourceField(p, _, _) => p.as_str(),
            };

            // Unknown provider check: virtual providers are also valid.
            let is_known = registry.provider_metadata(provider_name).is_some()
                || registry.is_virtual(provider_name);
            if !is_known {
                return Response::error(format!("unknown provider: {provider_name}"));
            }

            // Force evict: drop the cache entry so the normal miss path re-executes.
            if *force {
                if registry.is_virtual(provider_name) {
                    return Response::error(format!(
                        "cannot --force virtual provider '{provider_name}': no source to re-execute from"
                    ));
                }
                cache.remove(provider_name, effective_path.as_deref());
            }

            // Wait semantics: if the cached entry is stale, evict it so the normal
            // miss path below re-executes the provider inline and returns fresh data.
            // Skipped for virtual providers — they have no source to re-execute.
            if *wait
                && !*force
                && !registry.is_virtual(provider_name)
                && cache
                    .get_entry(provider_name, effective_path.as_deref())
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
                let src = if registry.is_virtual(provider_name) {
                    "virtual"
                } else if registry.provider_metadata(provider_name).is_some() {
                    "builtin"
                } else {
                    "unknown"
                };
                return Response::ok(serde_json::Value::String(src.to_string()), 0, false);
            }

            // Cache lookup with cold-miss inline-execute, routed by key parse form.
            // Canon §"Cold cache miss triggers inline fetch": on cache miss, synchronously
            // execute the relevant source(s), write to cache, then re-read.
            let (cache_hit, normal_response) = match &parsed {
                KeyParse::Field(provider, field) => {
                    // First check cache.
                    let mut hit = cache.get_field(provider, effective_path.as_deref(), field);
                    if hit.is_none()
                        && let Some(source_name) = registry.source_for_field(provider, field)
                    {
                        let source_name = source_name.to_string();
                        if inline_execute_source(
                            registry,
                            cache,
                            provider,
                            &source_name,
                            effective_path.as_deref(),
                        )
                        .await
                        {
                            hit = cache.get_field(provider, effective_path.as_deref(), field);
                        }
                    }
                    match hit {
                        Some((value, last_refreshed)) => {
                            let age_ms = last_refreshed.elapsed().as_millis();
                            let data =
                                serde_json::to_value(&value).unwrap_or(serde_json::Value::Null);
                            (true, Response::ok(data, age_ms, false))
                        }
                        None => {
                            // Distinguish nested-path not-found from full miss.
                            if field.contains('.') {
                                let head = field.split('.').next().unwrap_or(field.as_str());
                                if cache
                                    .get_field(provider, effective_path.as_deref(), head)
                                    .is_some()
                                {
                                    return Response::error(format!(
                                        "unknown field: {provider}.{field}"
                                    ));
                                }
                            }
                            (false, Response::miss())
                        }
                    }
                }
                KeyParse::Source(provider, source) => {
                    let mut hit = cache.get_source(provider, effective_path.as_deref(), source);
                    if hit.is_none()
                        && inline_execute_source(
                            registry,
                            cache,
                            provider,
                            source,
                            effective_path.as_deref(),
                        )
                        .await
                    {
                        hit = cache.get_source(provider, effective_path.as_deref(), source);
                    }
                    match hit {
                        Some(src_entry) => {
                            let age_ms = src_entry.age_ms();
                            let stale = src_entry.is_stale();
                            let data = serde_json::to_value(&src_entry.fields)
                                .unwrap_or(serde_json::Value::Null);
                            (true, Response::ok(data, age_ms, stale))
                        }
                        None => (false, Response::miss()),
                    }
                }
                KeyParse::SourceField(provider, source, field) => {
                    let mut hit = cache.get_source(provider, effective_path.as_deref(), source);
                    if hit.is_none()
                        && inline_execute_source(
                            registry,
                            cache,
                            provider,
                            source,
                            effective_path.as_deref(),
                        )
                        .await
                    {
                        hit = cache.get_source(provider, effective_path.as_deref(), source);
                    }
                    match hit {
                        Some(src_entry) => match src_entry.fields.get(field.as_str()) {
                            Some(value) => {
                                let age_ms = src_entry.age_ms();
                                let stale = src_entry.is_stale();
                                let data =
                                    serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
                                (true, Response::ok(data, age_ms, stale))
                            }
                            None => {
                                return Response::error(format!(
                                    "unknown field: {provider}.{source}.{field}"
                                ));
                            }
                        },
                        None => (false, Response::miss()),
                    }
                }
                KeyParse::Provider(provider) => {
                    // Whole-provider read: warm every applicable source on cold miss.
                    let mut hit = cache.get_entry(provider, effective_path.as_deref());
                    if hit.is_none()
                        && let Some(sources) = registry.provider_sources(provider)
                    {
                        let source_names: Vec<String> = sources
                            .iter()
                            .filter(|sm| match sm.scope {
                                SourceScope::Global => true,
                                SourceScope::PathScoped => effective_path.is_some(),
                            })
                            .map(|sm| sm.name.clone())
                            .collect();
                        for sn in &source_names {
                            inline_execute_source(
                                registry,
                                cache,
                                provider,
                                sn,
                                effective_path.as_deref(),
                            )
                            .await;
                        }
                        hit = cache.get_entry(provider, effective_path.as_deref());
                    }
                    match hit {
                        Some(entry) => {
                            let age_ms = entry.age_ms();
                            let stale = entry.is_stale();
                            let flat = entry.flatten_fields();
                            let data =
                                serde_json::to_value(&flat).unwrap_or(serde_json::Value::Null);
                            (true, Response::ok(data, age_ms, stale))
                        }
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
            let (provider_name, _field) = protocol::split_key(key);
            let requested = path.as_deref().or(context_path.as_deref());
            let effective_path = resolve_path(key, requested, registry);

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
                // Fallback: scheduler not available. Virtual providers are a no-op.
                if registry.is_virtual(provider_name) {
                    return Response {
                        ok: true,
                        data: None,
                        age_ms: None,
                        stale: None,
                        error: None,
                    };
                }
                // No providers are registered yet (Section H); return ok anyway
                // so callers don't see spurious errors during the build-up phase.
                if registry.provider_metadata(provider_name).is_none() {
                    return Response::error(format!("unknown provider: {provider_name}"));
                }
                Response {
                    ok: true,
                    data: None,
                    age_ms: None,
                    stale: None,
                    error: None,
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
            // Reject if a real (non-virtual) provider already owns this name.
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

            // Convert JSON object fields to provider Value map.
            let mut fields: HashMap<String, crate::provider::Value> = HashMap::new();
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
                fields.insert(field_key.clone(), value);
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

            // Write to cache under the synthetic "virtual" source name.
            cache.put_source(
                key,
                effective_path.as_deref(),
                "virtual",
                fields,
                interval_secs,
            );

            Response {
                ok: true,
                data: None,
                age_ms: None,
                stale: None,
                error: None,
            }
        }
        Request::Status => {
            let mut rows = cache.list_rows();

            let (lifecycle, failures) = if let Some(sched) = scheduler {
                let lc = sched.get_lifecycle_snapshots().await;
                let fs = sched.get_failure_states().await;
                (lc, fs)
            } else {
                (Default::default(), Default::default())
            };

            use crate::cache::RowKind;
            for row in rows.iter_mut() {
                let is_virtual = registry.is_virtual(&row.provider);

                if is_virtual {
                    row.kind = Some(RowKind::Virtual);
                } else {
                    // Look up the lifecycle snapshot for THIS row's owning source
                    // so glyphs and TTL columns reflect the source's strategy
                    // (refs vs diff vs status all differ within a single provider).
                    let triple = (row.provider.clone(), row.path.clone(), row.source.clone());
                    let matching_snap = lifecycle.get(&triple);

                    if let Some(snap) = matching_snap {
                        row.kind = Some(RowKind::Lifecycle {
                            decay: snap.decay,
                            watches_files: snap.watches_files,
                        });
                        // Only expose poll-related metadata when this source has
                        // a poll path. Pure Watch sources leave these None so the
                        // renderer doesn't fabricate `0s×00`.
                        if snap.poll_interval_secs > 0 {
                            row.poll_interval_secs = Some(snap.poll_interval_secs);
                            row.keep_alive_polls = Some(snap.keep_alive_polls);
                            row.polls_elapsed = Some(snap.polls_elapsed);
                        }
                        row.fsevents_reinstate = Some(snap.fsevents_reinstate);
                    } else {
                        row.kind = Some(RowKind::Transient);
                    }

                    // Failure backoff: per-source.
                    if let Some(snap) = failures.get(&triple) {
                        row.failure = Some(snap.clone());
                    }
                }
            }

            match serde_json::to_value(&rows) {
                Ok(v) => Response::ok(v, 0, false),
                Err(e) => Response::error(format!("serialization failed: {e}")),
            }
        }
        Request::Introspect {
            subject,
            duration_secs,
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
            IntrospectSubject::Providers => handle_introspect_providers(registry, scheduler).await,
            IntrospectSubject::Config => handle_introspect_config(config),
            IntrospectSubject::Cache => handle_introspect_cache(cache),
            IntrospectSubject::Lifecycle => handle_introspect_lifecycle(scheduler).await,
            IntrospectSubject::Watches => handle_introspect_watches(scheduler).await,
            IntrospectSubject::Timers => handle_introspect_timers(scheduler).await,
            IntrospectSubject::Demand => handle_introspect_demand(scheduler).await,
            IntrospectSubject::Procs => {
                let dur = *duration_secs;
                tokio::task::spawn_blocking(move || handle_introspect_procs(dur))
                    .await
                    .unwrap_or_else(|e| Response::error(format!("procs task panicked: {e}")))
            }
        },
        Request::Hello => {
            let data = serde_json::json!({
                "protocol_version": crate::protocol::PROTOCOL_VERSION,
                "daemon_version": env!("BEACHCOMBER_VERSION"),
            });
            Response::ok(data, 0, false)
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

    let mut verdicts = vec![serde_json::json!({"level": "PASS", "message": "daemon responsive"})];

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
        "version": env!("BEACHCOMBER_VERSION"),
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

fn summarize_invalidation(strategy: &crate::provider::InvalidationStrategy) -> String {
    use crate::provider::InvalidationStrategy;
    match strategy {
        InvalidationStrategy::Poll { interval_secs } => format!("poll {interval_secs}s"),
        InvalidationStrategy::Watch { patterns, .. } => {
            if patterns.is_empty() {
                "watch (abs_paths)".to_string()
            } else {
                format!("watch {}", patterns.join(","))
            }
        }
        InvalidationStrategy::WatchAndPoll {
            patterns,
            interval_secs,
            ..
        } => {
            let pats = if patterns.is_empty() {
                "(abs_paths)".to_string()
            } else {
                patterns.join(",")
            };
            format!("watch {pats} + poll {interval_secs}s")
        }
    }
}

async fn handle_introspect_providers(
    registry: &ProviderRegistry,
    scheduler: Option<&SchedulerHandle>,
) -> Response {
    let backoff_list: Vec<LifecycleInfo> = if let Some(s) = scheduler {
        s.get_status()
            .await
            .map(|st| st.lifecycle)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut providers_out = Vec::new();
    let mut verdicts = Vec::new();

    let mut names = registry.list();
    names.sort();

    for name in &names {
        let (is_virtual, is_real) = (
            registry.is_virtual(name),
            registry.provider_metadata(name).is_some(),
        );

        let source_type = if is_virtual {
            "virtual"
        } else if is_real {
            "builtin"
        } else {
            "unknown"
        };

        let (scope, sources_out, invalidation) = if let Some(meta) =
            registry.provider_metadata(name)
        {
            // Infer scope from per-source metadata.
            let any_path = meta
                .sources
                .iter()
                .any(|s| s.scope == SourceScope::PathScoped);
            let scope = if any_path { "path" } else { "global" };

            // Collect per-source info.
            let sources_out: Vec<serde_json::Value> = meta
                .sources
                .iter()
                .map(|sm| {
                    let fields: Vec<serde_json::Value> = sm
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
                    serde_json::json!({
                        "name": sm.name,
                        "scope": if sm.scope == SourceScope::PathScoped { "path" } else { "global" },
                        "fields": fields,
                        "invalidation": summarize_invalidation(&sm.invalidation),
                    })
                })
                .collect();

            // Use the first source's invalidation for the top-level summary.
            let invalidation = meta
                .sources
                .first()
                .map(|sm| summarize_invalidation(&sm.invalidation))
                .unwrap_or_else(|| "data-only".to_string());

            (scope, sources_out, invalidation)
        } else {
            ("global", Vec::new(), "data-only".to_string())
        };

        let relevant: Vec<&LifecycleInfo> = backoff_list
            .iter()
            .filter(|b| &b.provider == name)
            .collect();
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
            "source": source_type,
            "scope": scope,
            "sources": sources_out,
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

async fn handle_introspect_lifecycle(scheduler: Option<&SchedulerHandle>) -> Response {
    let lifecycle: Vec<LifecycleInfo> = if let Some(s) = scheduler {
        s.get_status()
            .await
            .map(|st| st.lifecycle)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut verdicts = Vec::new();
    if lifecycle.is_empty() {
        verdicts.push(serde_json::json!({"level": "PASS", "message": "no providers in decay"}));
    } else {
        for entry in &lifecycle {
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

    let lifecycle_json: Vec<serde_json::Value> = lifecycle
        .iter()
        .map(|b| serde_json::to_value(b).unwrap_or(serde_json::Value::Null))
        .collect();

    Response::ok(
        serde_json::json!({
            "lifecycle": lifecycle_json,
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
        s.get_status().await.map(|st| st.demand).unwrap_or_default()
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

fn handle_introspect_procs(duration_secs: Option<u64>) -> Response {
    let dur = duration_secs.unwrap_or(2);
    match crate::proc_snapshot::capture(dur) {
        Ok(result) => {
            let samples: Vec<serde_json::Value> = result
                .samples
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "command": s.command,
                        "count": s.count,
                        "category": s.category,
                    })
                })
                .collect();
            let suggestions: Vec<serde_json::Value> = result
                .replacement_suggestions
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "command_pattern": r.command_pattern,
                        "provider": r.provider,
                        "field": r.field,
                    })
                })
                .collect();
            let mut verdicts = vec![serde_json::json!({
                "level": "INFO",
                "message": format!("{}s sample: {} exec events", result.duration_secs, result.total),
            })];
            if !result.replacement_suggestions.is_empty() {
                verdicts.push(serde_json::json!({
                    "level": "WARN",
                    "message": format!(
                        "{} replacement opportunit{}: consider using `comb get` instead",
                        result.replacement_suggestions.len(),
                        if result.replacement_suggestions.len() == 1 { "y" } else { "ies" }
                    ),
                }));
            }
            Response::ok(
                serde_json::json!({
                    "duration_secs": result.duration_secs,
                    "samples": samples,
                    "replacement_suggestions": suggestions,
                    "verdicts": verdicts,
                }),
                0,
                false,
            )
        }
        Err(e) => Response::error(format!("procs snapshot failed: {e}")),
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
                    "error: {}\n\n",
                    response.error.as_deref().unwrap_or("unknown")
                );
            }
            match &response.data {
                Some(serde_json::Value::String(s)) => format!("{s}\n\n"),
                Some(serde_json::Value::Number(n)) => format!("{n}\n\n"),
                Some(serde_json::Value::Bool(b)) => format!("{b}\n\n"),
                Some(serde_json::Value::Object(map)) => {
                    // Emit `subkey=value` lines, sorted. Nested objects flatten
                    // as `outer.inner=value`. Matches
                    // docs/superpowers/specs/2026-04-21-code-review-fixes-design.md C9.
                    let mut lines: Vec<String> = map
                        .iter()
                        .flat_map(|(k, v)| {
                            if let serde_json::Value::Object(inner) = v {
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

#[cfg(test)]
mod parse_key_tests {
    use super::*;
    use crate::provider::registry::ProviderRegistry;
    use crate::provider::{
        FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
        ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope,
    };

    struct FakeSource(SourceMetadata);
    impl Source for FakeSource {
        fn metadata(&self) -> &SourceMetadata {
            &self.0
        }
        fn execute(&self, _path: Option<&str>) -> SourceResult {
            SourceResult::new()
        }
    }

    struct FakeProvider {
        name: String,
        source_name: String,
        fields: Vec<&'static str>,
    }

    impl Provider for FakeProvider {
        fn metadata(&self) -> ProviderMetadata {
            ProviderMetadata {
                name: self.name.clone(),
                sources: vec![SourceMetadata {
                    name: self.source_name.clone(),
                    fields: self
                        .fields
                        .iter()
                        .map(|&n| FieldSchema {
                            name: n.into(),
                            field_type: FieldType::String,
                        })
                        .collect(),
                    scope: SourceScope::Global,
                    invalidation: InvalidationStrategy::Poll { interval_secs: 30 },
                    keep_alive: KeepAlive::Polls(2),
                    failback: FailbackConfig {
                        reattempts: 3,
                        interval_secs: 30,
                    },
                    fsevents_reinstate: false,
                }],
            }
        }

        fn sources(&self) -> Vec<Box<dyn Source>> {
            vec![Box::new(FakeSource(self.metadata().sources[0].clone()))]
        }
    }

    fn registry_with_git() -> ProviderRegistry {
        let mut reg = ProviderRegistry::new();
        reg.register(Box::new(FakeProvider {
            name: "git".into(),
            source_name: "refs".into(),
            fields: vec!["branch", "sha"],
        }))
        .unwrap();
        reg
    }

    #[test]
    fn parse_key_recognises_provider() {
        let reg = registry_with_git();
        let k = parse_key("git", &reg);
        assert!(matches!(k, KeyParse::Provider(ref p) if p == "git"));
    }

    #[test]
    fn parse_key_recognises_source_when_registered() {
        let reg = registry_with_git();
        let k = parse_key("git.refs", &reg);
        assert!(matches!(k, KeyParse::Source(_, ref s) if s == "refs"));
    }

    #[test]
    fn parse_key_falls_back_to_field_when_no_such_source() {
        let reg = registry_with_git();
        let k = parse_key("git.branch", &reg);
        assert!(matches!(k, KeyParse::Field(_, ref f) if f == "branch"));
    }

    #[test]
    fn parse_key_recognises_source_field() {
        let reg = registry_with_git();
        let k = parse_key("git.refs.branch", &reg);
        assert!(
            matches!(k, KeyParse::SourceField(ref p, ref s, ref f) if p == "git" && s == "refs" && f == "branch")
        );
    }

    #[test]
    fn parse_key_unknown_provider_yields_field() {
        let reg = ProviderRegistry::new();
        // No providers registered: any 2-segment key falls to Field form.
        let k = parse_key("git.branch", &reg);
        assert!(matches!(k, KeyParse::Field(_, ref f) if f == "branch"));
    }
}

#[cfg(test)]
mod resolve_path_tests {
    use super::*;
    use crate::provider::registry::ProviderRegistry;
    use crate::provider::{
        FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
        ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope,
    };

    struct FakeSource(SourceMetadata);
    impl Source for FakeSource {
        fn metadata(&self) -> &SourceMetadata {
            &self.0
        }
        fn execute(&self, _path: Option<&str>) -> SourceResult {
            SourceResult::new()
        }
    }

    struct FakeProvider {
        name: String,
        scope: SourceScope,
    }

    impl Provider for FakeProvider {
        fn metadata(&self) -> ProviderMetadata {
            let (invalidation, keep_alive) = match self.scope {
                SourceScope::Global => (
                    InvalidationStrategy::Watch {
                        patterns: vec![],
                        abs_paths: vec![],
                    },
                    KeepAlive::Never,
                ),
                SourceScope::PathScoped => (
                    InvalidationStrategy::Poll { interval_secs: 30 },
                    KeepAlive::Polls(2),
                ),
            };
            ProviderMetadata {
                name: self.name.clone(),
                sources: vec![SourceMetadata {
                    name: "main".into(),
                    fields: vec![FieldSchema {
                        name: "v".into(),
                        field_type: FieldType::String,
                    }],
                    scope: self.scope,
                    invalidation,
                    keep_alive,
                    failback: FailbackConfig {
                        reattempts: 3,
                        interval_secs: 30,
                    },
                    fsevents_reinstate: false,
                }],
            }
        }

        fn sources(&self) -> Vec<Box<dyn Source>> {
            vec![Box::new(FakeSource(self.metadata().sources[0].clone()))]
        }
    }

    fn registry_with(providers: Vec<Box<dyn Provider>>) -> ProviderRegistry {
        let mut reg = ProviderRegistry::new();
        for p in providers {
            reg.register(p).unwrap();
        }
        reg
    }

    #[test]
    fn global_provider_ignores_explicit_path() {
        let reg = registry_with(vec![Box::new(FakeProvider {
            name: "hostname".into(),
            scope: SourceScope::Global,
        })]);
        let result = resolve_path("hostname", Some("/tmp"), &reg);
        assert_eq!(result, None, "global provider must ignore explicit path");
    }

    #[test]
    fn path_scoped_provider_honors_explicit_path() {
        let reg = registry_with(vec![Box::new(FakeProvider {
            name: "git".into(),
            scope: SourceScope::PathScoped,
        })]);
        let result = resolve_path("git", Some("/tmp"), &reg);
        // /tmp should canonicalize to something (may differ by OS)
        assert!(
            result.is_some(),
            "path-scoped provider should honor explicit path"
        );
    }

    #[test]
    fn unknown_provider_returns_none() {
        let reg = ProviderRegistry::new();
        let result = resolve_path("nonexistent", Some("/tmp"), &reg);
        // Unknown providers have no sources to declare PathScoped, so returns None.
        assert_eq!(result, None);
    }
}
