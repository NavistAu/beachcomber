//! One client connection: reading requests, writing answers, and dispatching each request to its op.

use super::Ctx;
use super::cache_facet::handle_layers;
use super::introspect::handle_introspect;
use super::put::{handle_memo, put};
use super::read::handle_get;
use super::refresh::handle_refresh;
use super::status::handle_status;
use super::watch::{WatchGuard, handle_watch};
use crate::protocol::{Request, Response};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// One client's connection: a request per line, an answer per request, until
/// the client closes it or a `watch` takes it over.
pub(super) async fn handle_connection(
    stream: tokio::net::UnixStream,
    ctx: Ctx,
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

        match serde_json::from_str::<Request>(trimmed) {
            Ok(Request::Watch { key, path }) => {
                ctx.requests_total.fetch_add(1, Ordering::Relaxed);
                // Watch takes over the connection — enter streaming mode
                let _open = WatchGuard::new(Arc::clone(&ctx.active_watches));
                handle_watch(&ctx, key, path, &mut reader, &mut writer).await;
                return Ok(());
            }
            Ok(request) => {
                ctx.requests_total.fetch_add(1, Ordering::Relaxed);
                let response = handle_request(&ctx, &request).await;
                let mut response_bytes = serde_json::to_string(&response).unwrap();
                response_bytes.push('\n');
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

/// Answer one request: the op's handler, over the registry in force.
async fn handle_request(ctx: &Ctx, request: &Request) -> Response {
    // One registry for the whole request: a reload part-way through cannot
    // mix two.
    let registry = &ctx.registry.current();
    match request {
        Request::Get {
            key,
            path,
            force,
            wait,
            log,
        } => handle_get(ctx, registry, key, path.as_deref(), *force, *wait, *log).await,
        Request::Refresh { key, path } => handle_refresh(ctx, registry, key, path.as_deref()).await,
        Request::Put {
            key,
            data,
            ttl,
            path,
            persist,
        } => {
            put(
                &ctx.cache,
                registry,
                &ctx.scheduler,
                key,
                data,
                ttl.as_deref(),
                path.as_deref(),
                *persist,
            )
            .await
        }
        Request::Memo {
            key,
            path,
            hash,
            data,
            watch,
            ttl,
        } => {
            handle_memo(
                ctx,
                key,
                path.as_deref(),
                hash,
                data.as_ref(),
                watch,
                ttl.as_deref(),
            )
            .await
        }
        Request::Status {} => handle_status(ctx, registry).await,
        Request::Introspect {
            subject,
            duration_secs,
        } => handle_introspect(ctx, registry, *subject, *duration_secs).await,
        Request::Layers { key, path } => handle_layers(ctx, registry, key, path.as_deref()).await,
        Request::Hello {} => {
            let data = serde_json::json!({
                "protocol_version": crate::protocol::PROTOCOL_VERSION,
                "daemon_version": env!("BEACHCOMBER_VERSION"),
            });
            Response::ok(data, 0)
        }
        // Watch is intercepted in handle_connection before reaching here
        Request::Watch { .. } => unreachable!("Watch handled before handle_request"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Cache;
    use crate::provider::registry::ProviderRegistry;
    use crate::scheduler::{SchedulerHandle, SchedulerMessage};
    use crate::server::Server;
    use crate::watcher_registry::WatcherRegistry;
    use tokio::io::{AsyncBufReadExt, BufReader};

    /// A scheduler that has stopped draining its queue does not cost a request
    /// its connection: `get` and `watch` are answered from the cache with the
    /// queue full, one [`crate::scheduler::SCHEDULER_WAIT`] later. Parked on
    /// that send for good, each kept its descriptor after its client had gone.
    #[tokio::test(start_paused = true)]
    async fn a_request_is_answered_with_the_schedulers_queue_full() {
        use tokio::io::AsyncWriteExt;
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("sock");
        let (tx, _never_drained) = tokio::sync::mpsc::channel(1);
        let handle = SchedulerHandle::new(tx);
        handle
            .send(SchedulerMessage::Refresh {
                provider: "p".into(),
                instances: Vec::new(),
            })
            .await;
        let cache = Arc::new(Cache::new());
        let registry = ProviderRegistry::new().share(&cache);
        Server::new(
            socket.clone(),
            cache,
            registry,
            handle,
            Arc::new(WatcherRegistry::new()),
        )
        .spawn()
        .unwrap();

        for request in [
            r#"{"op":"get","key":"hostname"}"#,
            r#"{"op":"watch","key":"hostname"}"#,
        ] {
            let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
            let (reader, mut writer) = stream.into_split();
            writer
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            let mut answer = String::new();
            BufReader::new(reader).read_line(&mut answer).await.unwrap();
            assert!(answer.contains(r#""ok":true"#), "{request}: {answer}");
        }
    }
}
