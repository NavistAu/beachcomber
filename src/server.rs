use crate::cache::Cache;
use crate::protocol::{self, Format, Request, Response};
use crate::provider::registry::ProviderRegistry;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{info, warn, debug};

pub struct Server {
    socket_path: PathBuf,
    cache: Arc<Cache>,
    registry: Arc<ProviderRegistry>,
}

impl Server {
    pub fn new(
        socket_path: PathBuf,
        cache: Arc<Cache>,
        registry: Arc<ProviderRegistry>,
    ) -> Self {
        Self { socket_path, cache, registry }
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
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, cache, registry).await {
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
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    while reader.read_line(&mut line).await? > 0 {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            line.clear();
            continue;
        }

        let response_bytes = match serde_json::from_str::<Request>(trimmed) {
            Ok(request) => {
                let response = handle_request(&request, &cache, &registry).await;
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

    Ok(())
}

async fn handle_request(
    request: &Request,
    cache: &Cache,
    registry: &ProviderRegistry,
) -> Response {
    match request {
        Request::Get { key, path, .. } => {
            let (provider_name, field) = protocol::split_key(key);

            if registry.get(provider_name).is_none() {
                return Response::error(format!("unknown provider: {}", provider_name));
            }

            match cache.get(provider_name, path.as_deref()) {
                Some(entry) => {
                    let age_ms = entry.age_ms();
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
                    Response::ok(data, age_ms, false)
                }
                None => Response::miss(),
            }
        }
        Request::Poke { key, path } => {
            let (provider_name, _) = protocol::split_key(key);

            match registry.get(provider_name) {
                Some(provider) => {
                    // TODO(plan2): Move provider execution to scheduler with spawn_blocking
                    // For MVP, hostname/user are instant and don't block meaningfully.
                    let path_clone = path.clone();
                    if let Some(result) = provider.execute(path_clone.as_deref()) {
                        cache.put(provider_name, path.as_deref(), result);
                    }
                    Response { ok: true, data: None, age_ms: None, stale: None, error: None }
                }
                None => Response::error(format!("unknown provider: {}", provider_name)),
            }
        }
        Request::Subscribe { .. } | Request::Unsubscribe { .. } => {
            Response { ok: true, data: None, age_ms: None, stale: None, error: None }
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
