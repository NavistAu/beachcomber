//! `watch`: a stream of frames over one key, until the client goes.

use super::Ctx;
use super::answer::read_watch_value;
use super::read::{all_instances, called_instances};
use crate::cache::Cache;
use crate::protocol::Response;
use crate::provider::registry::ProviderRegistry;
use crate::query::{Dir, Instance, QueryPlan};
use crate::scheduler::{SchedulerHandle, SchedulerMessage};
use crate::watcher_registry::WatcherRegistry;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, warn};

/// `watch`: the connection streams a frame whenever what `key` resolves to
/// changes, until the client goes.
pub(super) async fn handle_watch(
    ctx: &Ctx,
    key: String,
    path: Option<String>,
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
) {
    let (cache, handle, scheduler, watchers) =
        (&ctx.cache, &ctx.registry, &ctx.scheduler, &ctx.watchers);
    let requested = path.as_deref();
    // A reload puts another registry in force; the watch's plan was made under
    // the one before it, so it is made again. Subscribed before the first plan
    // is made, so a reload during it is not missed.
    let mut swapped = handle.subscribe();
    let registry = swapped.borrow_and_update().clone();
    let mut setup = match watch_setup(&key, requested, registry, scheduler, watchers).await {
        Ok(setup) => setup,
        Err(msg) => {
            let _ = write_watch_line(writer, &Response::error(msg)).await;
            return;
        }
    };

    // Send initial value
    let initial = setup.read(cache);
    if write_watch_line(writer, &initial).await.is_err() {
        return;
    }

    // Every half decides whether a frame is new. A template edited on its own
    // changes what the client renders just as much as a value does, and so does
    // a memo whose hash an invalidation dropped, so a frame whose only change
    // is one of those is still a frame.
    let mut last = frame_identity(&initial);

    // Stream loop. A watch owns its connection for its whole lifetime — after
    // the initial request the client never writes again — so any completion
    // of the read races the write-triggered path: `Ok(0)` EOF, an error, or
    // even `Ok(n>0)` (a protocol-violating extra write) all mean this
    // connection is done being useful, and the watch ends. This is what lets
    // a dead client's watch drop at once instead of lingering until the next
    // write to the watched key. ("At once" has one hole: a `sched.send` into
    // a full scheduler queue parks this task for up to `SCHEDULER_WAIT`.)
    //
    // The `notified()` future MUST be constructed inside the loop: a
    // `notify_one` landing mid-write stores a permit that the next fresh
    // `notified().await` consumes. Hoisting it out re-registers nothing and
    // becomes a genuine lost-wakeup bug.
    let mut probe = [0u8; 1];
    loop {
        tokio::select! {
            _ = setup.chain_watch.changed.notified() => {
                // Signal ongoing demand
                if !setup.layered {
                    demand_watched(scheduler, &setup.provider_name, &setup.instances, &setup.called).await;
                }
            }
            changed = swapped.changed() => {
                if changed.is_err() {
                    break;
                }
                let registry = swapped.borrow_and_update().clone();
                setup = match watch_setup(&key, requested, registry, scheduler, watchers).await {
                    Ok(setup) => setup,
                    Err(msg) => {
                        let _ = write_watch_line(writer, &Response::error(msg)).await;
                        break;
                    }
                };
            }
            r = reader.read(&mut probe) => {
                match r {
                    Ok(0) => debug!("watch ended: client closed the connection"),
                    Ok(n) => debug!("watch closed: client wrote {n} unexpected byte(s)"),
                    Err(e) => debug!("watch read ended: {e}"),
                }
                break;
            }
        }

        let response = setup.read(cache);

        // Field-level filtering: skip if no half changed.
        let current = frame_identity(&response);
        if current == last {
            continue;
        }
        last = current;

        if write_watch_line(writer, &response).await.is_err() {
            break; // Client disconnected
        }
    }
}

/// What decides whether a watch frame is new: its data, its template and its
/// memo half, then its vars and theirs. Ages are not compared: a frame is
/// about what changed, and an age always has.
pub(super) type FrameIdentity = (
    Option<serde_json::Value>,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
);

pub(super) fn frame_identity(frame: &Response) -> FrameIdentity {
    (
        frame.data.clone(),
        frame.template.clone(),
        frame
            .memo
            .as_ref()
            .map(|memo| libbeachcomber::protocol::memo_without_ages(frame.template.as_ref(), memo)),
        frame.var.clone(),
        frame
            .var_memo
            .as_ref()
            .map(|memo| libbeachcomber::protocol::memo_without_ages(frame.var.as_ref(), memo)),
    )
}

/// What a `watch` follows, made under one registry: the plan for its key, the
/// instances it reads and those its provider's lines call, and the
/// subscription to every entry the read walks. Made again when a reload puts
/// another registry in force.
pub(super) struct WatchSetup {
    registry: Arc<ProviderRegistry>,
    plan: QueryPlan,
    provider_name: String,
    requested_path: Dir,
    /// A layer facet is a plain read: a watch on one warms nothing.
    layered: bool,
    instances: Vec<Instance>,
    called: Vec<(String, Vec<Instance>)>,
    chain_watch: ChainWatch,
}

impl WatchSetup {
    fn read(&self, cache: &Cache) -> Response {
        read_watch_value(
            cache,
            &self.registry,
            &self.plan.target,
            &self.requested_path,
            &self.instances,
            self.plan.layers,
        )
    }
}

/// Make a watch's [`WatchSetup`] under `registry` and signal demand for what
/// it reads. The error is for the client.
pub(super) async fn watch_setup(
    key: &str,
    requested: Option<&str>,
    registry: Arc<ProviderRegistry>,
    scheduler: &SchedulerHandle,
    watchers: &Arc<WatcherRegistry>,
) -> Result<WatchSetup, String> {
    // The plan refuses a facet outside the closed set, as for every op.
    let plan = QueryPlan::build(key, requested, &registry)?;
    let provider_name = plan.provider.clone();

    // Both halves resolve up the chain from the directory the caller named; the
    // provider's own records sit at the instances the read reaches — see
    // [`read_watch_value`].
    let requested_path = plan.requested_path.clone();

    // A layer facet is a plain read: a watch on one warms nothing.
    let layered = plan.layers != crate::cache::Layers::All;

    // A watch follows the whole chain of every Source it reads: it cannot stop
    // at the first hit, because a later change can make a nearer instance miss.
    let instances = all_instances(&registry, &plan).await.inspect_err(|msg| {
        warn!("watch {key}: {msg}");
    })?;

    // The Sources the provider's lines call: their chains are followed too.
    let called = if layered || registry.lines().all_calls(&provider_name).is_empty() {
        Vec::new()
    } else {
        called_instances(&registry, &provider_name, &requested_path)
            .await
            .map(|(called, _)| called)
            .inspect_err(|msg| warn!("watch {key}: {msg}"))?
    };

    // Signal demand for every instance on the chains (source-aware: a field
    // demands only its owning Source).
    if !layered {
        demand_watched(scheduler, &provider_name, &instances, &called).await;
    }

    // Subscribe to every entry the read walk visits, not just the addressed
    // one: a `put` at an ancestor, at a directory below the provider's scope
    // entry, or at the global entry changes what this watch resolves to, so it
    // has to wake the stream. That is exactly what [`Cache::read_merged`]
    // walks (`query::read_chain`, which carries the roots of the instances even
    // when one sits above `$HOME`). Every write to `p` or to one of its facets
    // notifies `p` (the watcher registry's one key per namespace), so this one
    // chain also covers template edits and facet keys. One forwarder task per
    // link funnels the per-key broadcasts into a single channel.
    let chain_watch = {
        let changed = Arc::new(tokio::sync::Notify::new());
        let mut tasks = Vec::new();
        let mut links: Vec<(String, Dir)> = crate::query::read_chain(
            &requested_path,
            &crate::query::roots_of(&instances),
            registry.home(),
        )
        .into_iter()
        .map(|layer| (provider_name.clone(), layer))
        .collect();
        // A change to a Source the lines call changes what they give.
        for (called_provider, instances) in &called {
            links.extend(
                crate::query::read_chain(
                    &requested_path,
                    &crate::query::roots_of(instances),
                    registry.home(),
                )
                .into_iter()
                .map(|layer| (called_provider.clone(), layer)),
            );
        }
        for (namespace, layer) in links {
            let mut sub = watchers.subscribe(namespace.as_str(), layer.address());
            let changed = Arc::clone(&changed);
            tasks.push(tokio::spawn(async move {
                // A lagged subscriber has missed notifications, but one
                // "something changed" is all the stream loop needs — which is
                // also why a burst across several links collapses into one
                // wakeup rather than one re-read per notification.
                while let Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) =
                    sub.recv().await
                {
                    changed.notify_one();
                }
            }));
        }
        ChainWatch { tasks, changed }
    };

    Ok(WatchSetup {
        registry,
        plan,
        provider_name,
        requested_path,
        layered,
        instances,
        called,
        chain_watch,
    })
}

/// Signal a watch's demand: the instances it reads, and those of the Sources
/// its provider's lines call.
pub(super) async fn demand_watched(
    scheduler: &SchedulerHandle,
    provider: &str,
    instances: &[Instance],
    called: &[(String, Vec<Instance>)],
) {
    scheduler
        .send(SchedulerMessage::QueryActivity {
            provider: provider.to_string(),
            instances: instances.to_vec(),
        })
        .await;
    for (called_provider, instances) in called {
        scheduler
            .send(SchedulerMessage::QueryActivity {
                provider: called_provider.clone(),
                instances: instances.clone(),
            })
            .await;
    }
}

/// Counts one open `watch` for as long as its stream is live.
pub(super) struct WatchGuard(Arc<AtomicUsize>);

impl WatchGuard {
    pub(super) fn new(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Self(count)
    }
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The chain subscription a `watch` holds: one forwarder task per entry on the
/// chain, funnelling into `rx`. Dropping it stops the forwarders.
pub(super) struct ChainWatch {
    tasks: Vec<tokio::task::JoinHandle<()>>,
    changed: Arc<tokio::sync::Notify>,
}

impl Drop for ChainWatch {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

pub(super) async fn write_watch_line(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    response: &Response,
) -> Result<(), std::io::Error> {
    let mut line = serde_json::to_string(response).unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A watch frame is new when its data, its template or its memo half
    /// changed, and not when only an age did: a memo's `age_ms` in a group's
    /// tree as much as in a string's object, and never a key of the group that
    /// happens to be called `age_ms`.
    #[test]
    fn a_frame_is_new_when_a_half_changed_but_not_when_an_age_did() {
        use serde_json::json;
        let frame = |template: serde_json::Value, memo: serde_json::Value| Response {
            template: Some(template),
            memo: Some(memo),
            ..Response::miss()
        };
        let leaf = json!("{{ 1 }}");
        let at = |age: u64, hash: Option<&str>| {
            let mut m = json!({"stale": false, "age_ms": age, "data": 1});
            if let Some(h) = hash {
                m["hash"] = json!(h);
            }
            m
        };
        assert_eq!(
            frame_identity(&frame(leaf.clone(), at(1, Some("a")))),
            frame_identity(&frame(leaf.clone(), at(900, Some("a"))))
        );
        assert_ne!(
            frame_identity(&frame(leaf.clone(), at(1, Some("a")))),
            frame_identity(&frame(leaf.clone(), at(1, None))),
            "a dropped hash"
        );
        let group = json!({"age_ms": "{{ 1 }}", "g": {"h": "{{ 2 }}"}});
        let tree =
            |age: u64, h: u64| json!({"age_ms": at(age, Some("a")), "g": {"h": at(h, Some("b"))}});
        assert_eq!(
            frame_identity(&frame(group.clone(), tree(1, 2))),
            frame_identity(&frame(group.clone(), tree(3, 4)))
        );
        assert_ne!(
            frame_identity(&frame(group.clone(), tree(1, 2))),
            frame_identity(&frame(group.clone(), json!({"g": {"h": at(2, Some("b"))}}))),
            "a group's leaf named age_ms is a leaf, not an age"
        );
    }
}
