//! Building the config Sources of a composed `[daemon.providers]` table.
//!
//! Per-file rules (unknown keys, value types) are `seeded::read`'s, with the
//! file's label; every caller of [`build`] runs them too. A line of the wrong
//! type is therefore not reported here: the Source it belongs to is skipped.
//! What is checked here needs the composed tree: the prototype chain each
//! Source resolves through, its `return` chain rendered once into a schedule,
//! and the rules between Sources of one provider.

use crate::provider::SourceMetadata;
use crate::provider::config_source::{ConfigSourceSpec, ResultSpec};
use crate::scheduler::lifecycle::{
    DEFAULT_BUDGET_EXECUTE, DEFAULT_FAILBACK_COUNT, DEFAULT_FAILBACK_INTERVAL_SECS,
};
use crate::shape::decorators::{EngineDefaults, Recorded, record_return};
use crate::shape::prototype::{Kind, Link, chain, effective, source_prototypes, super_chain};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// A provider and the config Sources composed for it.
#[derive(Debug, Clone)]
pub struct ProviderDecl {
    pub name: String,
    pub sources: Vec<ConfigSourceSpec>,
}

/// The lines a `_` Source table holds itself. They apply to config Sources
/// only, and are checked as a Source's own lines are, once.
#[derive(Debug, Clone)]
pub struct PrototypeLines {
    /// `[daemon.providers.git.sources._]`
    pub path: String,
    pub root: Option<String>,
    pub data: Option<String>,
    pub result: Option<ResultSpec>,
}

const ENGINE_DEFAULTS: EngineDefaults = EngineDefaults {
    failback: crate::provider::FailbackConfig {
        reattempts: DEFAULT_FAILBACK_COUNT,
        interval_secs: DEFAULT_FAILBACK_INTERVAL_SECS,
    },
    budget_execute: DEFAULT_BUDGET_EXECUTE,
};

fn as_table(v: Option<&toml::Value>) -> Option<&toml::Table> {
    v.and_then(toml::Value::as_table)
}

/// What a line key holds: nothing, a string, or something else (the file
/// check's error, and the Source is skipped).
enum Line {
    Absent,
    Text(String),
    Wrong,
}

fn line(links: &[Link<'_>], key: &str) -> Line {
    match effective(links, key) {
        None => Line::Absent,
        Some(toml::Value::String(s)) => Line::Text(s.clone()),
        Some(_) => Line::Wrong,
    }
}

fn result_spec(value: &toml::Value) -> Option<ResultSpec> {
    match value {
        toml::Value::String(s) => Some(ResultSpec::Whole(s.clone())),
        toml::Value::Table(fields) => Some(ResultSpec::Fields(
            fields
                .iter()
                .map(|(name, line)| Some((name.clone(), line.as_str()?.to_string())))
                .collect::<Option<_>>()?,
        )),
        _ => None,
    }
}

/// Render the `return` chain of `links` and name the table that failed.
/// `Ok(None)` is a line that is not a string, for the file check to report.
fn check_return(links: &[Link<'_>], name: &str) -> Result<Option<Recorded>, String> {
    let lines = super_chain(links, "return");
    let mut strs: Vec<&str> = Vec::new();
    for (_, value) in &lines {
        match value.as_str() {
            Some(s) => strs.push(s),
            None => return Ok(None),
        }
    }
    // No return applies: poll and watch are opt-in, so the Source is the bare
    // result, watch-only, with the engine's failback and budget.
    if strs.is_empty() {
        strs.push("{{ result }}");
    }
    record_return(&strs, name, &ENGINE_DEFAULTS)
        .map(Some)
        .map_err(|e| {
            let at = lines.get(e.level).map_or(0, |(at, _)| *at);
            format!("{} return: {}", links[at].path, e.message)
        })
}

/// The lines each `_` Source table holds, for the caller to compile.
pub fn prototype_lines(providers: &toml::Table) -> Vec<PrototypeLines> {
    providers
        .iter()
        .filter_map(|(name, provider)| {
            let table = as_table(as_table(Some(provider))?.get("sources"))?;
            let proto = as_table(table.get("_"))?;
            let text = |key: &str| proto.get(key).and_then(|v| v.as_str()).map(String::from);
            Some(PrototypeLines {
                path: format!("[daemon.providers.{name}.sources._]"),
                root: text("root"),
                data: text("data"),
                result: proto.get("result").and_then(result_spec),
            })
        })
        .collect()
}

/// The config Sources `providers` (the composed `[daemon.providers]`) declare.
/// `existing` is every Source already registered, by provider: the built-in
/// Rust Sources, which a table cannot yet redefine, and whose fields a new
/// Source may not declare again.
pub fn build(
    providers: &toml::Table,
    existing: &HashMap<String, Vec<SourceMetadata>>,
) -> Result<Vec<ProviderDecl>, Vec<String>> {
    let mut errors: BTreeSet<String> = BTreeSet::new();
    let mut err = |e: String| {
        errors.insert(e);
    };

    // Every `_` return line loads, even one no Source's `| super` reaches.
    for name in providers.keys() {
        let links = source_prototypes(providers, name);
        if !links.is_empty()
            && let Err(e) = check_return(&links, "_")
        {
            err(e);
        }
    }

    let mut decls = Vec::new();
    for (provider, ptable) in providers {
        if provider == "_" {
            continue;
        }
        let Some(sources) = as_table(ptable.as_table().and_then(|p| p.get("sources"))) else {
            continue;
        };
        let builtin: Vec<&str> = existing
            .get(provider)
            .map(|v| v.iter().map(|s| s.name.as_str()).collect())
            .unwrap_or_default();
        let mut specs = Vec::new();
        for (source, stable) in sources {
            let Some(stable) = stable.as_table() else {
                continue;
            };
            if source == "_" {
                continue;
            }
            let at = format!("[daemon.providers.{provider}.sources.{source}]");
            if builtin.contains(&source.as_str()) {
                if ["root", "data", "result"]
                    .iter()
                    .any(|k| stable.contains_key(*k))
                {
                    err(format!(
                        "{provider}.{source} is a built-in Source: only return can be set until \
                         it is shipped as config"
                    ));
                }
                if stable.contains_key("return") {
                    err(format!("unknown key 'return' in {at}"));
                }
                continue;
            }
            let links = chain(providers, Kind::Source, provider, source);
            // A line of the wrong type is the file check's error: skip.
            let root = line(&links, "root");
            let data = line(&links, "data");
            let result = effective(&links, "result");
            if matches!(data, Line::Absent) {
                err(format!("{at} has no data line"));
            }
            if result.is_none() {
                err(format!(
                    "{at} has no result (a line, or result.<field> lines)"
                ));
            }
            let recorded = match check_return(&links, source) {
                Ok(recorded) => recorded,
                Err(e) => {
                    err(e);
                    None
                }
            };
            let (Line::Text(data), Some(Some(result)), Some(recorded)) =
                (data, result.map(result_spec), recorded)
            else {
                continue;
            };
            let root = match root {
                Line::Text(s) => Some(s),
                Line::Absent => None,
                Line::Wrong => continue,
            };
            specs.push(ConfigSourceSpec {
                provider: provider.clone(),
                name: source.clone(),
                root,
                data,
                result,
                recorded,
            });
        }
        if specs.is_empty() {
            continue;
        }
        // Two Sources of a provider may not declare one field.
        let mut owners: BTreeMap<String, String> = BTreeMap::new();
        for m in existing.get(provider).into_iter().flatten() {
            for f in m.fields.iter().filter(|f| !f.name.starts_with('<')) {
                owners.insert(f.name.clone(), m.name.clone());
            }
        }
        for spec in &specs {
            let ResultSpec::Fields(pairs) = &spec.result else {
                continue;
            };
            for (field, _) in pairs {
                if let Some(prev) = owners.insert(field.clone(), spec.name.clone()) {
                    err(format!(
                        "provider '{provider}' field '{field}' is declared by both Source \
                         '{prev}' and Source '{}'",
                        spec.name
                    ));
                }
            }
        }
        decls.push(ProviderDecl {
            name: provider.clone(),
            sources: specs,
        });
    }

    if errors.is_empty() {
        Ok(decls)
    } else {
        Err(errors.into_iter().collect())
    }
}
