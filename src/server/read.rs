//! `get`: bringing each demanded Source up to date along its instance chain, then answering from what is stored. `watch` reads the same chains.

use super::Ctx;
use super::answer::{Answered, answer_read, resolve_halves};
use super::cache_facet::cache_facet_response;
use crate::cache::{Cache, Layers, MergedRead};
use crate::protocol::{Response, Wait};
use crate::provider::registry::ProviderRegistry;
use crate::provider::source_call::instance_key;
use crate::query::{Ask, Found, Instance, InstanceWalk, Key, QueryPlan};
use crate::run::{Outcome, Want};
use crate::scheduler::SchedulerMessage;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

/// The root of `provider`'s Source `source` for `dir`
/// ([`ProviderRegistry::instance_root`]), and whether its line was rendered to
/// find it, without holding an async worker for a render: a config Source's
/// `root` line can run a command (`exec`, up to its timeout), fetch a URL or
/// call a plugin. The registry keeps each rendered root per directory while
/// the directories on its walk are as they were (one `stat` each, here), so
/// only the first request for a directory, and the first after a marker on
/// its walk came or went, pays for the render, on the blocking pool; a kept
/// root, and a test Source's own, resolve in place.
pub(super) async fn root_for(
    registry: &Arc<ProviderRegistry>,
    provider: &str,
    source: &str,
    dir: &str,
) -> Result<(Found, bool), String> {
    if registry.renders_root(provider, source) {
        if let Some(kept) = registry.kept_root(provider, source, dir) {
            return Ok((kept, false));
        }
        let (registry, provider, source, dir) = (
            Arc::clone(registry),
            provider.to_string(),
            source.to_string(),
            dir.to_string(),
        );
        return tokio::task::spawn_blocking(move || {
            registry.instance_root(&provider, &source, &dir)
        })
        .await
        .map_err(|e| format!("resolving the root failed: {e}"))?
        .map(|found| (found, true));
    }
    registry
        .instance_root(provider, source, dir)
        .map(|found| (found, false))
}

/// The roots found so far by one request, so Sources that find theirs the same
/// way ([`crate::provider::Source::root_key`]) ask once per directory.
#[derive(Default)]
pub(super) struct RootMemo {
    known: HashMap<(String, String, String), Found>,
    /// Whether any root was rendered, not answered from a kept one.
    rendered: bool,
}

/// The next instance of `walk`'s chain, or `None` when the chain is over.
pub(super) async fn next_instance(
    walk: &mut InstanceWalk,
    registry: &Arc<ProviderRegistry>,
    provider: &str,
    source: &str,
    memo: &mut RootMemo,
) -> Result<Option<Option<String>>, String> {
    let answer = match walk.ask() {
        Ask::Done => return Ok(None),
        Ask::Home => Found::Home,
        Ask::Root(dir) => {
            let how = registry
                .source(provider, source)
                .map_or_else(|| source.to_string(), |s| s.root_key());
            let key = (provider.to_string(), how, dir.clone());
            match memo.known.get(&key) {
                Some(known) => known.clone(),
                None => {
                    let (found, rendered) = root_for(registry, provider, source, &dir).await?;
                    memo.rendered |= rendered;
                    memo.known.insert(key, found.clone());
                    found
                }
            }
        }
    };
    walk.step(answer)
}

/// The Sources a plan reads, for the instances that matter to its layers: the
/// `put` layer is what `put` wrote, so no Source is involved.
pub(super) fn read_demand(plan: &QueryPlan) -> crate::query::SourceDemand {
    if plan.layers == Layers::Put {
        crate::query::SourceDemand::Sources(Vec::new())
    } else {
        plan.demand.clone()
    }
}

/// Every instance of every Source `plan` reads, each Source's chain in full:
/// what a read that cannot stop at the first hit (a whole namespace, a `watch`,
/// the `:cache` report) consults. An error is a `root` that could not be worked
/// out.
pub(super) async fn all_instances(
    registry: &Arc<ProviderRegistry>,
    plan: &QueryPlan,
) -> Result<Vec<Instance>, String> {
    let mut out = Vec::new();
    let mut memo = RootMemo::default();
    for source in crate::query::demanded_source_names(registry, &plan.provider, &read_demand(plan))
    {
        let dir = plan.requested_path.as_deref();
        out.extend(chain(registry, &plan.provider, &source, dir, &mut memo).await?);
    }
    Ok(out)
}

/// Every instance of `provider`'s Source `source` a read from `dir` reaches,
/// nearest first, its roots found through `memo`.
async fn chain(
    registry: &Arc<ProviderRegistry>,
    provider: &str,
    source: &str,
    dir: Option<&str>,
    memo: &mut RootMemo,
) -> Result<Vec<Instance>, String> {
    let mut walk = registry.instance_walk(provider, source, dir);
    let mut found = Vec::new();
    while let Some(root) = next_instance(&mut walk, registry, provider, source, memo).await? {
        found.push(Instance {
            source: source.to_string(),
            root,
        });
    }
    Ok(found)
}

/// What a read asks of each instance it reaches.
#[derive(Clone, Copy, Debug)]
pub(super) struct Read {
    /// `--force`: a run that begins now, past the failback backoff.
    force: bool,
    /// How long the read waits for each run it needs.
    wait: Wait,
    /// When the read began: every wait counts from here.
    began: tokio::time::Instant,
}

impl Read {
    fn new(force: bool, wait: Wait) -> Self {
        Self {
            force,
            wait,
            began: tokio::time::Instant::now(),
        }
    }

    /// Until when the read waits for a run, given what it waits for when the
    /// call says nothing (`budget`: `Some` for a record it has, `None`, the
    /// run's end, for a cold one or a forced run); `None` is to the run's
    /// end.
    fn until(&self, budget: Option<Duration>) -> Option<tokio::time::Instant> {
        match self.wait {
            Wait::Budget => budget.map(|budget| self.began + budget),
            Wait::For(wait) => Some(self.began + wait),
            Wait::Forever => None,
        }
    }
}

/// A run a read started or took, and how long it waits for it: `None` is to
/// its end.
pub(super) struct Pending {
    run: crate::run::Run,
    until: Option<tokio::time::Instant>,
}

/// Start the run, or take the one in flight, that one instance of a Source
/// needs for `read`, and say how long the read waits for it. `None` when it
/// needs none: its record is valid (within its lifetime, its stat inputs as
/// its run found them), or it is in failback backoff (only `--force` runs past
/// that). `root` is the instance: the cache entry its records live at.
///
/// | The read finds | It runs | It waits for |
/// |---|---|---|
/// | a valid record | nothing | nothing |
/// | a record whose inputs changed | the run in flight if it sees them as they are, or one | the call's `wait`, else the Source's budget |
/// | a record past its lifetime (`stale`) | the run in flight, or one | as above |
/// | nothing stored | the run in flight, or one | the call's `wait`, else the run's end |
/// | anything, with `--force` | one that begins now | as above |
///
/// The check stats the record's inputs on the request task: one `stat` per
/// input, bounded by their count, not by time.
///
/// The decision is per instance, never per provider: a warm sibling says
/// nothing about this one. A run replaces the instance's map when it succeeds
/// and leaves it when it fails, so the static records sharing the entry, and
/// the last good value, stand either way.
pub(super) fn start_for_read(
    registry: &Arc<ProviderRegistry>,
    cache: &Arc<Cache>,
    provider: &str,
    source_name: &str,
    root: Option<&str>,
    read: Read,
) -> Option<Pending> {
    let source = registry.source(provider, source_name)?;
    let want = if read.force {
        Want::Forced
    } else {
        // A valid instance is the common read: answered before anything
        // shared is locked.
        Want::for_read(cache.record_state(provider, root, source_name))?
    };
    // A record it has is waited for up to the Source's budget; a cold one,
    // or a forced run, to the run's end.
    let until = read.until(
        matches!(want, Want::Changed | Want::Any).then(|| source.metadata().schedule.budget),
    );
    let key = instance_key(provider, source_name, root);
    let run = registry.runs().start(want, registry, cache, &key)?;
    Some(Pending { run, until })
}

/// A read of an instance whose Source watches its tree waits until the file
/// events from before it have arrived: a change in the tree, which no stat
/// sees, is then in the record's state for [`start_for_read`] to find.
pub(super) async fn hear_tree(
    registry: &Arc<ProviderRegistry>,
    cache: &Arc<Cache>,
    provider: &str,
    source_name: &str,
    root: Option<&str>,
    read: Read,
) {
    if read.force {
        return;
    }
    if let Some(barrier) =
        crate::provider::source_call::tree_barrier(registry, cache, provider, source_name, root)
    {
        barrier.pass(read.began).await;
    }
}

/// Wait for each of `pending`, all under way together, until its deadline:
/// whether any ended in time. A run that is not over by then goes on and
/// records itself; the read answers with what is stored.
///
/// A run that failed is a miss (its failure is logged where it is recorded),
/// and the read goes on to the rest of the chain. One that failed because a
/// Source it calls failed, or called itself, is a fault in the lines, not in
/// the world: the read shows it, as it shows a `root` that fails.
pub(super) async fn settle(pending: Vec<Pending>) -> Result<bool, String> {
    let mut ended = false;
    for Pending { run, until } in pending {
        let outcome = match until {
            None => Some(run.outcome().await),
            Some(deadline) => run.outcome_by(deadline).await,
        };
        match outcome {
            Some(Outcome::Failed {
                message,
                fault: true,
            }) => return Err(message),
            Some(_) => ended = true,
            None => {}
        }
    }
    Ok(ended)
}

/// The instances, by provider, of the Sources `provider`'s `result` and
/// `return` lines call, as a read from `dir` reaches them, and whether a `root`
/// was rendered to find them, not answered from a kept one. A `root` that
/// needs rendering is rendered here, off the async workers.
pub(super) async fn called_instances(
    registry: &Arc<ProviderRegistry>,
    provider: &str,
    dir: Option<&str>,
) -> Result<(Vec<(String, Vec<Instance>)>, bool), String> {
    let called: Vec<(String, String)> = registry.lines().all_calls(provider).to_vec();
    let mut out: Vec<(String, Vec<Instance>)> = Vec::new();
    let mut memo = RootMemo::default();
    for (called_provider, source) in &called {
        // A disabled provider has no instances: its Source is not run for a
        // call, and the call is a miss.
        if registry.is_disabled(called_provider) {
            continue;
        }
        let found = chain(registry, called_provider, source, dir, &mut memo).await?;
        match out.iter_mut().find(|(p, _)| p == called_provider) {
            Some((_, instances)) => instances.extend(found),
            None => out.push((called_provider.clone(), found)),
        }
    }
    Ok((out, memo.rendered))
}

/// Bring up to date, as `read` asks, every Source of another provider that
/// `provider`'s `result` and `return` lines call, at each instance a read from
/// `dir` reaches (their runs started together, then waited for), and say
/// which those were and whether the read must be made again: a run ended, or
/// a root was resolved that the first read did not have. What the lines then
/// render over is stored. These calls are part of the read: its budget
/// applies to them.
pub(super) async fn warm_called(
    registry: &Arc<ProviderRegistry>,
    cache: &Arc<Cache>,
    provider: &str,
    dir: Option<&str>,
    read: Read,
) -> Result<(Vec<(String, Vec<Instance>)>, bool), String> {
    let (called, resolved) = called_instances(registry, provider, dir).await?;
    for (called_provider, instances) in &called {
        for instance in instances {
            let root = instance.root.as_deref();
            hear_tree(
                registry,
                cache,
                called_provider,
                &instance.source,
                root,
                read,
            )
            .await;
        }
    }
    let pending: Vec<Pending> = called
        .iter()
        .flat_map(|(called_provider, instances)| {
            instances.iter().filter_map(move |instance| {
                start_for_read(
                    registry,
                    cache,
                    called_provider,
                    &instance.source,
                    instance.root.as_deref(),
                    read,
                )
            })
        })
        .collect();
    let ended = settle(pending).await?;
    Ok((called, resolved || ended))
}

/// What a climb does at each instance it reaches.
#[derive(Clone, Copy)]
pub(super) enum Climb {
    /// Bring the instance up to date as the read asks, then read.
    Warm(Read),
    /// Only find the instances; run nothing (`refresh` hands them to the
    /// scheduler).
    Select,
}

/// Where a climb got to.
pub(super) struct Climbed {
    /// The instances brought up to date and demanded, nearest first: every
    /// one visited but those a `put` answered.
    pub(super) reached: Vec<Instance>,
    /// Every instance visited, one a `put` answered included (which is not in
    /// `reached`: nothing runs or is kept warm for it).
    visited: Vec<Instance>,
    /// The merged read at the end.
    merged: Option<MergedRead>,
}

/// One demanded Source's walk up its instances, with the next root found but
/// not yet visited.
pub(super) struct Walking {
    name: String,
    walk: InstanceWalk,
    next: Option<Option<String>>,
    over: bool,
}

/// How near a root is to the directory read: deeper is nearer, and the
/// instance at `~` is the farthest of all.
pub(super) fn nearness(root: &Option<String>) -> i64 {
    match root {
        Some(root) => std::path::Path::new(root).components().count() as i64,
        None => -1,
    }
}

/// Whether a merged read answers a field read: only a leaf does. An object is
/// the union of what each instance says, so it is complete only at `~`, the
/// same as a namespace read.
pub(super) fn answers_field(merged: &Option<MergedRead>, target: &Key) -> bool {
    target.is_field() && lookup_merged(merged, &target.path).is_some_and(|v| !v.is_object())
}

/// The instance chains of every demanded Source, climbed together nearest
/// first, until the merged read answers or every chain is over.
///
/// Every Source is asked for its next root before any instance is visited, and
/// the nearest root among them is visited next, `~` last. A Source whose next
/// root is far cannot therefore answer before another Source's nearer one.
/// After each visit the merged read is tried: a field read stops on a leaf; a
/// namespace read, and a field that is an object, take every level.
///
/// Each instance's run starts as the climb reaches it, and the read waits for
/// the runs of a level together before it tries the merged read there. A read
/// that cannot stop early (a namespace, or a provider whose lines decide what
/// exists) starts every run it needs before it waits for any.
///
/// `honor_put` lets a field a `put` states at an instance be answered there,
/// without running the Source it hides.
pub(super) async fn climb(
    registry: &Arc<ProviderRegistry>,
    cache: &Arc<Cache>,
    plan: &QueryPlan,
    demanded: &[String],
    mode: Climb,
    honor_put: bool,
) -> Result<Climbed, String> {
    let provider = plan.provider.as_str();
    // Under the shipped lines a field is its Source's, and a `put` or a hit
    // settles it early. Under any other a line decides what exists, so a read
    // takes every level and honours no put's shortcut.
    let by_default = registry.reads_by_default(provider);
    let honor_put = honor_put && by_default;
    let mut walking: Vec<Walking> = demanded
        .iter()
        .map(|name| Walking {
            name: name.clone(),
            walk: registry.instance_walk(provider, name, plan.requested_path.as_deref()),
            next: None,
            over: false,
        })
        .collect();
    let mut memo = RootMemo::default();
    let mut out = Climbed {
        reached: Vec::new(),
        visited: Vec::new(),
        merged: None,
    };
    let stops_early = by_default && plan.target.is_field();
    let mut pending = Vec::new();
    loop {
        for w in walking.iter_mut() {
            if w.next.is_none() && !w.over {
                match next_instance(&mut w.walk, registry, provider, &w.name, &mut memo).await? {
                    Some(root) => w.next = Some(root),
                    None => w.over = true,
                }
            }
        }
        let nearest = walking
            .iter()
            .filter_map(|w| w.next.as_ref().map(nearness))
            .max();
        let Some(nearest) = nearest else {
            if !pending.is_empty() || out.merged.is_none() {
                settle(std::mem::take(&mut pending)).await?;
                out.merged = read_chain_merged(cache, registry, plan, &out.visited);
            }
            return Ok(out);
        };
        for w in walking.iter_mut() {
            if w.next.as_ref().is_none_or(|root| nearness(root) != nearest) {
                continue;
            }
            let root = w.next.take().expect("checked above");
            out.visited.push(Instance {
                source: w.name.clone(),
                root: root.clone(),
            });
            if honor_put
                && plan.target.is_field()
                && cache.put_answers(provider, root.as_deref(), plan.target.rest())
            {
                continue;
            }
            if let Climb::Warm(read) = mode {
                hear_tree(registry, cache, provider, &w.name, root.as_deref(), read).await;
                pending.extend(start_for_read(
                    registry,
                    cache,
                    provider,
                    &w.name,
                    root.as_deref(),
                    read,
                ));
            }
            out.reached.push(Instance {
                source: w.name.clone(),
                root,
            });
        }
        if stops_early {
            settle(std::mem::take(&mut pending)).await?;
            out.merged = read_chain_merged(cache, registry, plan, &out.visited);
            if answers_field(&out.merged, &plan.target) {
                return Ok(out);
            }
        }
    }
}

/// The merged read of `plan`'s address over the `visited` instances.
pub(super) fn read_chain_merged(
    cache: &Cache,
    registry: &ProviderRegistry,
    plan: &QueryPlan,
    visited: &[Instance],
) -> Option<MergedRead> {
    cache.read_merged(
        &plan.provider,
        plan.requested_path.as_deref(),
        visited,
        registry,
        plan.layers,
    )
}

/// The value at `field` in a merged read, as wire JSON.
///
/// `provider::Value` is `#[serde(untagged)]` over JSON-native shapes, so the
/// conversion cannot fail; `.ok()` rather than a `Null` stand-in so that if it
/// somehow did, the address reads as absent instead of as a stored null. Every
/// conversion out of a merged tree takes that line.
pub(super) fn lookup_merged(
    merged: &Option<MergedRead>,
    path: &[String],
) -> Option<serde_json::Value> {
    let value = crate::provider::lookup_segments(&merged.as_ref()?.fields, path)?;
    serde_json::to_value(value).ok()
}

/// `get`: the key's plan, then each demanded Source brought up to date along
/// its instance chain, nearest first, and the halves assembled from what is
/// stored.
pub(super) async fn handle_get(
    ctx: &Ctx,
    registry: &Arc<ProviderRegistry>,
    key: &str,
    path: Option<&str>,
    force: bool,
    wait: Wait,
) -> Response {
    let (cache, scheduler) = (&ctx.cache, &ctx.scheduler);
    // One source-aware plan drives the whole request: provider, resolved
    // path, parsed target, and the warming demand. Get and Watch both
    // build this so they share identical key semantics.
    let requested = path;
    let plan = match QueryPlan::build(key, requested, registry) {
        Ok(plan) => plan,
        Err(msg) => return Response::error(msg),
    };
    let provider_name = plan.provider.as_str();

    // `p:cache` reports on what is cached: nothing is warmed, no demand
    // is signalled, and no template is attached. It reports the records
    // a read would find, so it follows every chain in full.
    if plan.layers == crate::cache::Layers::Cache {
        let instances = match all_instances(registry, &plan).await {
            Ok(instances) => instances,
            Err(msg) => {
                warn!("get {key}: {msg}");
                return Response::error(msg);
            }
        };
        return cache_facet_response(
            cache,
            registry,
            &plan.target,
            plan.requested_path.as_deref(),
            &instances,
        );
    }

    // The two halves are addressed from different places. A template is
    // a static record, and static records resolve up the chain from the
    // directory the caller is standing in — so the template half reads
    // from the requested path, which the plan already canonicalised. The
    // data half reads the instances the walk below reaches.
    let template_path = plan.requested_path.as_deref();

    // `--force` re-executes below, per instance; nothing is evicted
    // here. Evicting the addressed entry wholesale used to take the
    // static records with it.
    if force && registry.is_disabled(provider_name) {
        return Response::error(format!(
            "cannot --force disabled provider '{provider_name}': no source to re-execute \
             (enabled = false)"
        ));
    }

    // Every Source the key demands is brought up to date on its own
    // terms — one cold source is no longer covered for by a warm
    // sibling — and each at its instances, nearest first. `plan.demand`
    // is every Source for a bare namespace and the owning Source for a
    // field, which is what makes this one loop serve both — and what
    // makes it the same set `refresh` re-executes. The `put` layer is
    // what `put` wrote: no Source has anything to run for it.
    let demanded =
        crate::query::demanded_source_names(registry, provider_name, &read_demand(&plan));
    // How long the read waits for each run it needs: its budgets
    // count from now.
    let read = Read::new(force, wait);

    // The instance chains, climbed together nearest first ([`climb`]):
    // each instance is brought up to date and the merged read asked for
    // the address. A field read stops on a leaf; a namespace read has no
    // single address to satisfy, so it takes every level.
    let Climbed {
        reached,
        visited,
        mut merged,
    } = match climb(
        registry,
        cache,
        &plan,
        &demanded,
        Climb::Warm(read),
        !force && plan.layers != crate::cache::Layers::Source,
    )
    .await
    {
        Ok(climbed) => climbed,
        Err(msg) => {
            warn!("get {key}: {msg}");
            return Response::error(msg);
        }
    };

    // The Sources the provider's lines call are brought up to date
    // before the lines render, which do no IO: a cold one runs here,
    // once. After the provider's own, so a callee that reads the
    // provider (`comb.<p>`) finds it warm. A provider whose lines call
    // nothing pays nothing.
    let called =
        if plan.layers != crate::cache::Layers::Put && registry.lines().has_calls(provider_name) {
            match warm_called(
                registry,
                cache,
                provider_name,
                plan.requested_path.as_deref(),
                read,
            )
            .await
            {
                Ok((called, changed)) => {
                    // The first read of the lines found no root resolved
                    // for the called Sources, or them cold: read again.
                    if changed {
                        merged = read_chain_merged(cache, registry, &plan, &visited);
                    }
                    called
                }
                Err(msg) => {
                    warn!("get {key}: {msg}");
                    return Response::error(msg);
                }
            }
        } else {
            Vec::new()
        };

    // A field the Source's last run left absent is absent until it runs
    // again, as a field it produced is present until then: the read is
    // answered from that run, never by running the Source once more.

    // Demand every instance the read reached, so each is kept warm and,
    // the first time, its schedule is registered. The Sources the
    // lines call are kept warm the same way, while they are called.
    for (called_provider, instances) in called {
        scheduler
            .send(SchedulerMessage::QueryActivity {
                provider: called_provider,
                instances,
            })
            .await;
    }
    if !reached.is_empty() {
        scheduler
            .send(SchedulerMessage::QueryActivity {
                provider: provider_name.to_string(),
                instances: reached,
            })
            .await;
    }

    // The template half is what the provider's `return` attaches over
    // the data read: `get box.widgets.foo` where `box.widgets` is an
    // object without a `foo` is a hit when a template answers there, a
    // miss otherwise. Both halves, assembled the one way `watch`
    // assembles them.
    let Answered { merged, half } = answer_read(
        cache,
        registry,
        &plan.target,
        template_path,
        plan.layers,
        merged,
    );
    // `--force` computes the value afresh: a Source re-executes, and
    // a template renders past its memo on the client. A static
    // namespace has no Source, so only a template half gives a forced
    // read something to do.
    if force && half.template.is_none() && registry.is_static_namespace(provider_name) {
        return Response::error(format!(
            "cannot --force static namespace '{provider_name}': no source to re-execute from"
        ));
    }
    resolve_halves(&merged, &plan.target, half)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config Source whose run appends to `<dir>/runs`, touches
    /// `<dir>/started`, then waits for `<dir>/go` and answers 1.
    fn gated(dir: &std::path::Path) -> (Arc<ProviderRegistry>, Arc<Cache>) {
        let cache = Arc::new(Cache::new());
        let registry = ProviderRegistry::with_config(
            &crate::config_tests::with_shipped_defaults(&format!(
                "[daemon.providers.demo.sources.s]\n\
                 data = \"{{{{ exec(['sh', '-c', 'echo run >> {d}/runs; touch {d}/started; \
                 while [ ! -f {d}/go ]; do sleep 0.01; done; echo 1']) | trim }}}}\"\n\
                 result.v = \"{{{{ data }}}}\"\n",
                d = dir.display()
            )),
            &std::env::vars().collect(),
        )
        .unwrap()
        .share(&cache);
        (registry, cache)
    }

    /// A read of the instance at `~` of `demo.s`, as a `get` makes it.
    async fn read_of(registry: &Arc<ProviderRegistry>, cache: &Arc<Cache>) -> Result<bool, String> {
        let pending = start_for_read(
            registry,
            cache,
            "demo",
            "s",
            None,
            Read::new(false, Wait::Budget),
        );
        settle(pending.into_iter().collect()).await
    }

    async fn until(what: &str, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "never: {what}");
            tokio::task::yield_now().await;
        }
    }

    /// A read that finds its instance cold while a run of it is under way
    /// (here a poll's) takes that run rather than running the Source beside it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_read_takes_the_run_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, cache) = gated(dir.path());
        let key = instance_key("demo", "s", None);
        let poll = registry
            .runs()
            .start(Want::Any, &registry, &cache, &key)
            .unwrap();
        until("the poll's run to begin", || {
            dir.path().join("started").exists()
        })
        .await;

        let read = tokio::spawn({
            let (registry, cache) = (registry.clone(), cache.clone());
            async move { read_of(&registry, &cache).await }
        });
        until("the read to take the poll's run", || {
            registry.runs().callers(&key) == Some(2)
        })
        .await;
        std::fs::write(dir.path().join("go"), "").unwrap();
        assert_eq!(read.await.unwrap(), Ok(true), "the read waited for a run");
        assert_eq!(poll.outcome().await, Outcome::Stored);
        let runs = std::fs::read_to_string(dir.path().join("runs")).unwrap();
        assert_eq!(runs.lines().count(), 1, "the Source ran once, for both");
    }

    /// A read that stops waiting part-way through the run it started (its
    /// connection gone, say) takes nothing with it: the run records itself.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_read_that_stops_waiting_takes_nothing_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, cache) = gated(dir.path());
        {
            let read = read_of(&registry, &cache);
            tokio::pin!(read);
            tokio::select! {
                done = &mut read => panic!("the gated run ended: {done:?}"),
                () = until("the read's run to begin", || dir.path().join("started").exists()) => {}
            }
        }
        std::fs::write(dir.path().join("go"), "").unwrap();
        until("the run to store its record", || {
            cache.has_provider_records_for("demo", None, "s")
        })
        .await;
        assert!(registry.runs().in_flight().is_empty());
    }

    /// The provider `g` of one Source `s`, whose run is held at its start
    /// until the test sends on its channel (as a process slow to spawn would
    /// hold it), and then answers 1.
    struct SlowStart(Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>);

    impl crate::provider::Source for SlowStart {
        fn metadata(&self) -> &crate::provider::SourceMetadata {
            static META: std::sync::OnceLock<crate::provider::SourceMetadata> =
                std::sync::OnceLock::new();
            META.get_or_init(|| crate::provider::SourceMetadata {
                name: "s".into(),
                fields: vec![crate::provider::FieldSchema { name: "n".into() }],
                schedule: crate::provider::Schedule {
                    failback: crate::provider::FailbackConfig {
                        reattempts: 3,
                        interval_secs: 3600,
                    },
                    budget: Duration::from_secs(3600),
                    timeout: Duration::from_secs(600),
                    ..crate::provider::Schedule::poll(60, 2)
                },
            })
        }

        fn execute(&self, _: Option<&str>) -> Result<crate::provider::SourceResult, String> {
            self.0.lock().unwrap().recv().map_err(|e| e.to_string())?;
            let mut result = crate::provider::SourceResult::new();
            result.insert("n", crate::provider::Value::Int(1));
            Ok(result)
        }
    }

    impl SlowStart {
        fn sources(&self) -> Vec<Box<dyn crate::provider::Source>> {
            vec![Box::new(SlowStart(Arc::clone(&self.0)))]
        }
    }

    /// A `--wait 0` read starts the run it needs and is answered on its first
    /// poll, however long the run takes to get going: it waits on nothing,
    /// the run and the runtime's timer (which turns in whole milliseconds)
    /// alike. Nothing ended in time, so the read answers what is stored; the
    /// run goes on and records itself. One worker, so nothing turns the timer
    /// between the read's start and its poll.
    #[tokio::test]
    async fn a_wait_0_read_is_answered_at_once_with_its_run_held_at_its_start() {
        let (go, held) = std::sync::mpsc::channel();
        let mut registry = ProviderRegistry::new();
        registry
            .register_sources(
                "g",
                SlowStart(Arc::new(std::sync::Mutex::new(held))).sources(),
            )
            .unwrap();
        let (registry, cache) = (Arc::new(registry), Arc::new(Cache::new()));
        let read = Read::new(false, Wait::For(Duration::ZERO));
        let pending =
            start_for_read(&registry, &cache, "g", "s", None, read).expect("a cold instance runs");
        let run = pending.run.clone();
        let answered = tokio_test::task::spawn(settle(vec![pending])).poll();
        go.send(()).unwrap();
        assert_eq!(
            answered,
            std::task::Poll::Ready(Ok(false)),
            "answered at once, no run ended"
        );
        assert_eq!(run.outcome().await, Outcome::Stored);
    }
}
