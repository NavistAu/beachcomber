//! The poll of every provider's and Source's `enabled.template` (a Source's
//! named `p.s`, [`crate::provider::gate`]). A task of its own, off the
//! scheduler's loop: each check renders on the blocking pool, holding no lock
//! and nothing but the registry it took, and one slow check delays only
//! itself. When a check says other than the registry in force, the scheduler
//! is told ([`SchedulerMessage::Switch`]), and it switches the provider in
//! place. A check that fails changes nothing; its error is kept for
//! `comb check providers` and logged once until it changes.

use crate::provider::registry::RegistryHandle;
use crate::scheduler::{SchedulerHandle, SchedulerMessage};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// How often the task looks for a check that is due: polls are whole seconds.
const TICK: Duration = Duration::from_secs(1);

/// Poll the checks of the registry in force in `registry` until `cancel`.
pub fn spawn_gate_poll(
    registry: RegistryHandle,
    scheduler: SchedulerHandle,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Each check's next render; the load rendered every one just now.
        let mut due: HashMap<String, Instant> = HashMap::new();
        let mut running: HashSet<String> = HashSet::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tick.tick() => {
                    let current = registry.current();
                    let gates = current.gates();
                    due.retain(|name, _| gates.contains_key(name));
                    let now = Instant::now();
                    for (name, gate) in gates {
                        let next = due.entry(name.clone()).or_insert(now + gate.poll);
                        if now < *next || running.contains(name) {
                            continue;
                        }
                        *next = now + gate.poll;
                        running.insert(name.clone());
                        let (name, template, vars) =
                            (name.clone(), gate.template.clone(), current.vars().clone());
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            let rendered = tokio::task::spawn_blocking({
                                let (name, template) = (name.clone(), template.clone());
                                move || crate::provider::gate::render(&name, &template, &vars)
                            })
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()));
                            let _ = tx.send((name, template, rendered));
                        });
                    }
                }
                Some((name, template, rendered)) = rx.recv() => {
                    running.remove(&name);
                    let current = registry.current();
                    // A reload in the meantime rendered its own.
                    if current.gates().get(&name).is_none_or(|g| g.template != template) {
                        continue;
                    }
                    match rendered {
                        Ok(on) => {
                            current.note_gate(&name, Ok(()));
                            if current.is_switched_off(&name) == on {
                                scheduler
                                    .send(SchedulerMessage::Switch {
                                        provider: name,
                                        on,
                                        registry: registry.clone(),
                                    })
                                    .await;
                            }
                        }
                        Err(e) => {
                            if current.note_gate(&name, Err(e.clone())) {
                                tracing::warn!("{name} enabled.template: {e}; left as it was");
                            }
                        }
                    }
                }
            }
        }
    })
}
