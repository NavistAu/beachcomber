//! `introspect`: the daemon's view of itself, one handler per subject.

use super::Ctx;
use crate::boundaries::proc_snap::ProcessSnapshotter;
use crate::cache::Cache;
use crate::config::Config;
use crate::protocol::{IntrospectSubject, Response};
use crate::provider::registry::ProviderRegistry;
use crate::scheduler::{DemandInfo, LifecycleInfo, PollTimerInfo, SchedulerHandle};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// `introspect daemon`: the daemon's own health.
async fn handle_introspect_daemon(ctx: &Ctx) -> Response {
    // The runs in flight, the watch backend and the watches that failed come
    // from the scheduler's status, when it answers.
    let (in_flight_count, watch_backend, watches) =
        if let Some(sched_status) = ctx.scheduler.get_status().await {
            (
                sched_status.in_flight.len() as u64,
                sched_status.watch_backend,
                sched_status.watches,
            )
        } else {
            (0, "unknown".to_string(), Vec::new())
        };
    let active_watchers = ctx.active_watches.load(Ordering::Relaxed) as u64;
    let cache_entries = ctx.cache.len() as u64;
    let (socket_path, start_instant, requests_total) =
        (&ctx.socket_path, ctx.start_instant, &ctx.requests_total);
    let (reaper, loaded_config) = (ctx.reaper.as_deref(), ctx.config.loaded_path());
    let watch_backend = watch_backend.as_str();
    let watches = watches.as_slice();
    use crate::protocol::{DaemonHealth, ReaperStatus, Verdict};
    let verdict = |level: &str, message: String| Verdict {
        level: level.to_string(),
        message,
    };

    let uptime_secs = start_instant.elapsed().as_secs();

    let mut verdicts = vec![verdict("PASS", "daemon responsive".to_string())];

    if in_flight_count > 50 {
        verdicts.push(verdict(
            "WARN",
            format!("{in_flight_count} in-flight requests (threshold 50)"),
        ));
    } else {
        verdicts.push(verdict("PASS", format!("in_flight={in_flight_count}")));
    }

    // Watch degradation is observable.
    if watch_backend == "native" {
        verdicts.push(verdict(
            "PASS",
            "watch backend: native fs events".to_string(),
        ));
    } else {
        verdicts.push(verdict("WARN", format!(
                "watch backend: {watch_backend} — kernel fs events undelivered; watch invalidation degraded to polling"
            )));
    }
    for failed in failed_watches(watches) {
        verdicts.push(verdict("WARN", failed));
    }

    // Reaper capability is never silently degraded. `reaper` is None for
    // embedded/test servers (field omitted).
    let reaper = reaper.map(|health| {
        use std::sync::atomic::Ordering::Relaxed;
        let armed = health.armed.load(Relaxed);
        let visibility_ok = health.visibility_ok.load(Relaxed);
        let kill_denied = health.kill_denied_total.load(Relaxed);

        if !armed {
            verdicts.push(verdict("PASS", "reaper: not armed (side daemon)".to_string()));
        } else if visibility_ok {
            verdicts.push(verdict("PASS", "reaper: armed, system-wide process visibility".to_string()));
        } else {
            verdicts.push(verdict("WARN", "reaper visibility degraded: PID 1 not visible in process enumeration — this daemon runs confined (sandbox-spawned?) and cannot police daemons outside its confinement; restart it from an unconfined shell".to_string()));
        }
        if armed && kill_denied > 0 {
            verdicts.push(verdict("WARN", format!(
                    "reaper: {kill_denied} kill attempt(s) denied by the OS — orphans visible but unsignalable"
                )));
        }

        ReaperStatus {
            armed,
            visibility: if visibility_ok { "system-wide" } else { "confined" }.to_string(),
            sweeps: health.sweeps_total.load(Relaxed),
            reaped: health.reaped_total.load(Relaxed),
            kill_denied,
        }
    });

    let health = DaemonHealth {
        pid: i64::from(std::process::id()),
        version: env!("BEACHCOMBER_VERSION").to_string(),
        uptime_secs,
        socket_path: socket_path.to_string_lossy().into_owned(),
        config_path: loaded_config.map(|p| p.to_string_lossy().into_owned()),
        requests_total: requests_total.load(Ordering::Relaxed),
        in_flight: in_flight_count,
        active_watchers,
        cache_entries,
        watch_backend: watch_backend.to_string(),
        reaper,
        verdicts,
    };

    Response::ok(
        serde_json::to_value(health).expect("daemon health serialises"),
        0,
    )
}

pub(super) fn summarize_invalidation(strategy: &crate::provider::InvalidationStrategy) -> String {
    use crate::provider::InvalidationStrategy;
    match strategy {
        InvalidationStrategy::Poll {
            interval_secs,
            count: 0,
        } => format!("ttl {interval_secs}s"),
        InvalidationStrategy::Poll { interval_secs, .. } => format!("poll {interval_secs}s"),
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
            count,
            ..
        } => {
            let pats = if patterns.is_empty() {
                "(abs_paths)".to_string()
            } else {
                patterns.join(",")
            };
            let schedule = if *count == 0 { "ttl" } else { "poll" };
            format!("watch {pats} + {schedule} {interval_secs}s")
        }
    }
}

pub(super) async fn handle_introspect_providers(
    registry: &ProviderRegistry,
    scheduler: &SchedulerHandle,
) -> Response {
    let backoff_list: Vec<LifecycleInfo> = scheduler
        .get_status()
        .await
        .map(|st| st.lifecycle)
        .unwrap_or_default();
    let failures = scheduler.get_failure_states().await;

    let mut providers_out = Vec::new();
    let mut verdicts = Vec::new();

    let mut names = registry.list();
    names.sort();

    for name in &names {
        let (is_static, is_real) = (
            registry.is_static_namespace(name),
            registry.provider_metadata(name).is_some(),
        );

        let sourceless = registry
            .provider_metadata(name)
            .is_some_and(|m| m.sources.is_empty());
        let source_type = if is_static || sourceless {
            "static"
        } else if is_real {
            "source"
        } else {
            "unknown"
        };

        let disabled = registry.is_disabled(name);
        let (sources_out, invalidation) = if let Some(meta) = registry.provider_metadata(name) {
            // Collect per-source info.
            let sources_out: Vec<serde_json::Value> = meta
                .sources
                .iter()
                .map(|sm| {
                    let fields: Vec<serde_json::Value> = sm
                        .fields
                        .iter()
                        .map(|f| serde_json::json!({"name": f.name}))
                        .collect();
                    serde_json::json!({
                        "name": sm.name,
                        "fields": fields,
                        "invalidation": if disabled {
                            "disabled".to_string()
                        } else {
                            summarize_invalidation(&sm.invalidation)
                        },
                    })
                })
                .collect();

            // Use the first source's invalidation for the top-level summary.
            let invalidation = if disabled {
                "disabled".to_string()
            } else {
                meta.sources
                    .first()
                    .map(|sm| summarize_invalidation(&sm.invalidation))
                    .unwrap_or_else(|| "data-only".to_string())
            };

            (sources_out, invalidation)
        } else {
            (Vec::new(), "data-only".to_string())
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
                "message": format!("{name} in backoff {} (stage={})", secs(worst.elapsed_secs), worst.stage)
            }));
        }

        // A Source whose latest runs failed, whether or not one ever succeeded.
        // One verdict per Source: the instance with the longest streak.
        let mut failing: Vec<(&str, u32, Option<&str>)> = failures
            .iter()
            .filter(|(k, _)| &k.provider == name)
            .map(|(k, f)| {
                (
                    k.source.as_str(),
                    f.consecutive_failures,
                    f.last_error.as_deref(),
                )
            })
            .collect();
        failing.sort_by(|a, b| a.0.cmp(b.0).then(b.1.cmp(&a.1)));
        failing.dedup_by_key(|(source, _, _)| *source);
        for (source, n, error) in &failing {
            let why = error.map_or(String::new(), |e| format!(": {e}"));
            verdicts.push(serde_json::json!({
                "level": "WARN",
                "message": format!("{name}.{source} failing, {n} run(s) in a row{why}"),
            }));
        }
        let failing = (!failing.is_empty()).then(|| {
            serde_json::json!({
                "sources": failing.iter().map(|(s, _, _)| s).collect::<Vec<_>>(),
                "consecutive_failures": failing.iter().map(|(_, n, _)| n).max(),
            })
        });

        providers_out.push(serde_json::json!({
            "name": name,
            "failing": failing,
            "source": source_type,
            "sources": sources_out,
            "invalidation": invalidation,
            "in_backoff": in_backoff,
            "disabled": disabled,
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
    )
}

pub(super) fn handle_introspect_config(config: &Config, registry: &ProviderRegistry) -> Response {
    let path_json = match config.loaded_path() {
        Some(p) => serde_json::Value::String(p.display().to_string()),
        None => serde_json::Value::Null,
    };
    let source_count = registry.config_source_count() as u64;
    let verdicts = vec![
        serde_json::json!({"level": "PASS", "message": "config parsed"}),
        serde_json::json!({"level": "PASS", "message": format!("{source_count} config Sources loaded")}),
    ];
    Response::ok(
        serde_json::json!({
            "path": path_json,
            "parsed": true,
            "errors": [],
            "config_source_count": source_count,
            "verdicts": verdicts,
        }),
        0,
    )
}

pub(super) fn handle_introspect_cache(cache: &Cache) -> Response {
    let entries = cache.list_entries();
    let total = entries.len() as u64;
    let verdicts = vec![serde_json::json!({
        "level": "PASS",
        "message": format!("{total} entries"),
    })];

    Response::ok(
        serde_json::json!({
            "total_entries": total,
            "verdicts": verdicts,
        }),
        0,
    )
}

pub(super) async fn handle_introspect_lifecycle(scheduler: &SchedulerHandle) -> Response {
    let lifecycle: Vec<LifecycleInfo> = scheduler
        .get_status()
        .await
        .map(|st| st.lifecycle)
        .unwrap_or_default();

    let mut verdicts = Vec::new();
    if lifecycle.is_empty() {
        verdicts.push(serde_json::json!({"level": "PASS", "message": "no providers in decay"}));
    } else {
        for entry in &lifecycle {
            // A static address is named by its node: `kubecontext.context`.
            let mut label = entry.provider.clone();
            if !entry.node.is_empty() {
                label = format!("{label}.{}", entry.node);
            }
            if let Some(p) = &entry.path {
                label = format!("{label} ({p})");
            }
            verdicts.push(serde_json::json!({
                "level": "WARN",
                "message": format!("{label} — stage={} elapsed={}", entry.stage, secs(entry.elapsed_secs)),
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
    )
}

pub(super) async fn handle_introspect_watches(scheduler: &SchedulerHandle) -> Response {
    let watches = scheduler
        .get_status()
        .await
        .map(|st| st.watches)
        .unwrap_or_default();

    let verdicts = watch_verdicts(&watches);

    Response::ok(
        serde_json::json!({
            "watches": watches,
            "verdicts": verdicts,
        }),
        0,
    )
}

/// How long a path may be `pending` before the worker is called slow: a
/// registration can take seconds, and a few are normal.
pub(super) const PENDING_TOO_LONG_SECS: u64 = 30;

/// What a WARN verdict says of each watch whose registration failed. The next
/// demand of an instance that watches it asks for it again.
pub(super) fn failed_watches(
    watches: &[crate::watcher::Watch],
) -> impl Iterator<Item = String> + '_ {
    watches.iter().filter_map(|w| match &w.state {
        crate::watcher::WatchState::Failed { error } => Some(format!(
            "watch of {} failed: {error}; the next read of a Source that watches it tries again",
            w.path.display()
        )),
        _ => None,
    })
}

/// What a WARN verdict says of each watch pending past [`PENDING_TOO_LONG_SECS`].
pub(super) fn slow_watches(watches: &[crate::watcher::Watch]) -> impl Iterator<Item = String> + '_ {
    watches.iter().filter_map(|w| match &w.state {
        crate::watcher::WatchState::Pending if w.asked_secs_ago >= PENDING_TOO_LONG_SECS => {
            Some(format!(
                "watch of {} still registering after {}",
                w.path.display(),
                secs(w.asked_secs_ago)
            ))
        }
        _ => None,
    })
}

/// The verdicts on the watch table, which holds a path for each watch an
/// Active instance declares and none otherwise (a watch exists only while its
/// instance is Active). So an empty table is an idle daemon, healthy; the
/// warnings are a declared watch that is not registered: one that failed, and
/// one still pending after [`PENDING_TOO_LONG_SECS`].
pub(super) fn watch_verdicts(watches: &[crate::watcher::Watch]) -> Vec<serde_json::Value> {
    let mut verdicts = vec![if watches.is_empty() {
        serde_json::json!({
            "level": "PASS",
            "message": "idle: no instance is Active, so no path is watched",
        })
    } else {
        serde_json::json!({"level": "PASS", "message": format!("watching {} paths", watches.len())})
    }];
    verdicts.extend(
        failed_watches(watches)
            .chain(slow_watches(watches))
            .map(|message| serde_json::json!({"level": "WARN", "message": message})),
    );
    verdicts
}

pub(super) async fn handle_introspect_timers(scheduler: &SchedulerHandle) -> Response {
    let timers: Vec<PollTimerInfo> = scheduler
        .get_status()
        .await
        .map(|st| st.poll_timers)
        .unwrap_or_default();

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
                "message": format!("{label} overdue (interval={}, last={} ago)", secs(t.interval_secs), secs(t.last_run_secs_ago)),
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
    )
}

pub(super) async fn handle_introspect_demand(scheduler: &SchedulerHandle) -> Response {
    let demand: Vec<DemandInfo> = scheduler
        .get_status()
        .await
        .map(|st| st.demand)
        .unwrap_or_default();

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
    )
}

pub(super) fn handle_introspect_procs(
    procs: &dyn ProcessSnapshotter,
    duration_secs: Option<u64>,
) -> Response {
    let dur = duration_secs.unwrap_or(2);
    match procs.capture(dur) {
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
            )
        }
        // The machine cannot trace process starts (no eslogger privileges,
        // an unsupported platform): a missing capability, not comb failing.
        Err(e) => Response::ok(
            serde_json::json!({
                "duration_secs": dur,
                "samples": [],
                "replacement_suggestions": [],
                "verdicts": [{
                    "level": "INFO",
                    "message": format!("process sampling unavailable: {e}"),
                }],
            }),
            0,
        ),
    }
}

/// A count of seconds, as every duration comb shows reads.
pub(super) fn secs(n: u64) -> String {
    libbeachcomber::render::duration(std::time::Duration::from_secs(n))
}

/// `introspect <subject>`.
pub(super) async fn handle_introspect(
    ctx: &Ctx,
    registry: &Arc<ProviderRegistry>,
    subject: IntrospectSubject,
    duration_secs: Option<u64>,
) -> Response {
    let scheduler = &ctx.scheduler;
    match subject {
        IntrospectSubject::Daemon => handle_introspect_daemon(ctx).await,
        IntrospectSubject::Providers => handle_introspect_providers(registry, scheduler).await,
        IntrospectSubject::Config => handle_introspect_config(&ctx.config, registry),
        IntrospectSubject::Cache => handle_introspect_cache(&ctx.cache),
        IntrospectSubject::Lifecycle => handle_introspect_lifecycle(scheduler).await,
        IntrospectSubject::Watches => handle_introspect_watches(scheduler).await,
        IntrospectSubject::Timers => handle_introspect_timers(scheduler).await,
        IntrospectSubject::Demand => handle_introspect_demand(scheduler).await,
        IntrospectSubject::Procs => {
            let procs = Arc::clone(&ctx.procs);
            tokio::task::spawn_blocking(move || {
                handle_introspect_procs(procs.as_ref(), duration_secs)
            })
            .await
            .unwrap_or_else(|e| Response::error(format!("procs task panicked: {e}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch(
        path: &str,
        state: crate::watcher::WatchState,
        asked_secs_ago: u64,
    ) -> crate::watcher::Watch {
        crate::watcher::Watch {
            path: path.into(),
            state,
            asked_secs_ago,
        }
    }

    fn levels(verdicts: &[serde_json::Value]) -> Vec<&str> {
        verdicts
            .iter()
            .map(|v| v["level"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn no_watches_is_an_idle_daemon_not_a_warning() {
        let verdicts = watch_verdicts(&[]);
        assert_eq!(levels(&verdicts), ["PASS"]);
        assert!(verdicts[0]["message"].as_str().unwrap().contains("idle"));
    }

    #[test]
    fn a_failed_watch_warns_and_a_registered_one_does_not() {
        use crate::watcher::WatchState::{Active, Failed};
        let verdicts = watch_verdicts(&[
            watch("/a", Active { via: None }, 900),
            watch(
                "/b",
                Failed {
                    error: "gone".into(),
                },
                1,
            ),
        ]);
        assert_eq!(levels(&verdicts), ["PASS", "WARN"]);
        assert!(verdicts[1]["message"].as_str().unwrap().contains("/b"));
    }

    #[test]
    fn a_pending_watch_warns_only_once_it_has_waited_too_long() {
        use crate::watcher::WatchState::Pending;
        assert_eq!(
            levels(&watch_verdicts(&[watch(
                "/a",
                Pending,
                PENDING_TOO_LONG_SECS - 1
            )])),
            ["PASS"]
        );
        assert_eq!(
            levels(&watch_verdicts(&[watch(
                "/a",
                Pending,
                PENDING_TOO_LONG_SECS
            )])),
            ["PASS", "WARN"]
        );
    }
}
