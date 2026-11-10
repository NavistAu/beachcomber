//! `refresh`: re-executing the Sources a key demands, in the scheduler.

use super::Ctx;
use super::read::{Climb, climb};
use crate::protocol::Response;
use crate::provider::registry::ProviderRegistry;
use crate::query::QueryPlan;
use crate::scheduler::SchedulerMessage;
use std::sync::Arc;
use tracing::warn;

/// `refresh`: re-execute the Sources a key demands, each at the instances a
/// `get --force` of the same key would re-run, in the scheduler.
pub(super) async fn handle_refresh(
    ctx: &Ctx,
    registry: &Arc<ProviderRegistry>,
    key: &str,
    path: Option<&str>,
) -> Response {
    let (cache, scheduler) = (&ctx.cache, &ctx.scheduler);
    // The same plan every other op builds, so the key is checked and
    // the path resolved exactly as for a `get`.
    let requested = path;
    let plan = match QueryPlan::build(key, requested, registry) {
        Ok(plan) => plan,
        Err(msg) => return Response::error(msg),
    };
    let provider_name = plan.provider.as_str();
    if plan.layers != crate::cache::Layers::All {
        return Response::error(format!("cannot refresh {key}: a layer facet is read-only"));
    }

    // The same set `get` warms, re-executed instead of read. A key that
    // names none — a static namespace, a facet, an unknown name, or a
    // field no Source owns — has nothing to re-execute, and saying ok
    // would claim work that never happened. Reading configuration
    // afresh is not a refresh: the config watcher does that.
    let demanded = crate::query::demanded_source_names(registry, provider_name, &plan.demand);
    let addressed = libbeachcomber::key::join(&plan.target.provider, plan.target.rest());
    if registry.is_disabled(provider_name) {
        return Response::error(format!("nothing to refresh: {provider_name} is disabled"));
    }
    if let Some(off) = crate::query::only_disabled_sources(registry, provider_name, &plan.demand) {
        return Response::error(format!("nothing to refresh: {off} is disabled"));
    }
    // A cached template under the key renders afresh at its next read,
    // wherever its memos are: their hashes go. That is a refresh too.
    let memos = cache.drop_memo_hashes_under(provider_name, plan.target.rest());
    if demanded.is_empty() {
        if memos {
            return Response::miss();
        }
        return Response::error(format!("nothing to refresh: {addressed} has no sources"));
    }

    // The instances a `get --force` of the same key would re-run: every
    // instance for a namespace, and for a field those up to the one that
    // answers it. Found the way `get` finds them.
    let instances = match climb(registry, cache, &plan, &demanded, Climb::Select, false).await {
        Ok(climbed) => climbed.reached,
        Err(msg) => {
            warn!("refresh {key}: {msg}");
            return Response::error(msg);
        }
    };
    // Every demanded Source is nearest and found no instance here.
    if instances.is_empty() {
        if memos {
            return Response::miss();
        }
        return Response::error(format!(
            "nothing to refresh: {addressed} has no instance at {}",
            plan.requested_path.address().unwrap_or("~")
        ));
    }
    scheduler
        .send(SchedulerMessage::Refresh {
            provider: provider_name.to_string(),
            instances,
        })
        .await;
    Response::miss()
}
