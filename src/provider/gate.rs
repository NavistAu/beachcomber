//! A provider's or a Source's `enabled.template`: a check, rendered at load
//! and on its poll, that switches it off when it renders false. It reads files
//! and the clock and runs nothing: the `Pure`, `Clock` and `Io` tiers
//! ([`Surface::Check`]).
//!
//! A check is named by what it switches: `p` a provider, `p.s` its Source `s`
//! (a provider name holds no `.`). The poll, the switch and the reasons
//! `comb check providers` gives are the same for both.

use crate::shape::prototype::{Kind, chain};
use libbeachcomber::engine::{EngineCtx, engine_env, with_ctx};
use libbeachcomber::eval::{Known, Line, Probe, eval_line_with, lint, used_names};
use libbeachcomber::library::{Surface, first_refused, underscore_with};
use minijinja::value::{Serde, Value as MjValue};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// How often a check is rendered when neither the provider nor `_` says.
pub const DEFAULT_POLL: Duration = Duration::from_secs(30);

/// One provider's or Source's check.
#[derive(Debug, Clone, PartialEq)]
pub struct Gate {
    pub template: String,
    pub poll: Duration,
}

/// Every check `providers` (a composed `[daemon.providers]`) declares, with
/// its poll: the provider's own `enabled.poll`, else `_`'s, else
/// [`DEFAULT_POLL`]. A Source's is its own, else its prototypes' (`<p>.sources._`,
/// `_.sources._`), else the abstract provider's, else [`DEFAULT_POLL`]. The file
/// check has refused a malformed one.
pub fn gates(providers: &toml::Table) -> HashMap<String, Gate> {
    let poll_of = |name: &str| {
        providers
            .get(name)?
            .get("enabled")?
            .get("poll")
            .and_then(crate::config::duration_value)
    };
    let fallback = poll_of("_").unwrap_or(DEFAULT_POLL);
    let mut out: HashMap<String, Gate> = providers
        .keys()
        .filter_map(|name| {
            let template = crate::shape::provider::gate_in(providers, name)?;
            Some((
                name.clone(),
                Gate {
                    template: template.to_string(),
                    poll: poll_of(name).unwrap_or(fallback),
                },
            ))
        })
        .collect();
    for (provider, source, enabled) in source_switches(providers) {
        let Some(template) = enabled.get("template").and_then(toml::Value::as_str) else {
            continue;
        };
        let links = chain(providers, Kind::Source, provider, source);
        let poll = links
            .iter()
            .find_map(|l| l.table.get("enabled")?.get("poll"))
            .and_then(crate::config::duration_value)
            .unwrap_or(fallback);
        out.insert(
            format!("{provider}.{source}"),
            Gate {
                template: template.to_string(),
                poll,
            },
        );
    }
    out
}

/// Every named Source's own `enabled` among `providers`, never a `_`'s.
fn source_switches(providers: &toml::Table) -> Vec<(&str, &str, &toml::Value)> {
    let mut out = Vec::new();
    for (provider, table) in providers.iter().filter(|(p, _)| *p != "_") {
        let Some(sources) = table.get("sources").and_then(toml::Value::as_table) else {
            continue;
        };
        for (source, table) in sources.iter().filter(|(s, _)| *s != "_") {
            if let Some(enabled) = table.get("enabled") {
                out.push((provider.as_str(), source.as_str(), enabled));
            }
        }
    }
    out
}

/// The Sources `providers` switch off for good (`enabled = false`), as `p.s`.
pub fn sources_off(providers: &toml::Table) -> Vec<String> {
    source_switches(providers)
        .into_iter()
        .filter(|(_, _, enabled)| enabled.as_bool() == Some(false))
        .map(|(p, s, _)| format!("{p}.{s}"))
        .collect()
}

/// The table a check named `name` is written in.
pub fn path_of(name: &str) -> String {
    match name.split_once('.') {
        Some((p, s)) => format!("[daemon.providers.{p}.sources.{s}]"),
        None => format!("[daemon.providers.{name}]"),
    }
}

/// The load errors of `name`'s check: a call to anything that runs a process,
/// fetches or calls a plugin, a name it cannot read (it sees `env` and `_`),
/// or text that does not compile.
pub fn lint_gate(probe: &Probe<'_>, name: &str, template: &str) -> Vec<String> {
    let at = format!("{} enabled.template", path_of(name));
    let used = used_names(template);
    if let Some(item) = first_refused(Surface::Check, |n| used.contains_key(n)) {
        return vec![format!(
            "{at}: {} is not available in a check: it may read files and the clock, and runs \
             nothing",
            item.name
        )];
    }
    lint(probe, template, Line::Root, &Known::default())
        .into_iter()
        .map(|f| format!("{at}: {f}"))
        .collect()
}

/// What a load reads checks in.
pub fn probe() -> Probe<'static> {
    Probe::new(engine_env())
}

/// Render `name`'s check with `vars` as its `env`: whether the provider or
/// Source is on. `none`, `false`, `0` and an empty string, list or table are
/// off. `_.name` is the provider's name, or the Source's, as in its lines.
pub fn render(name: &str, template: &str, vars: &HashMap<String, String>) -> Result<bool, String> {
    let jinja = engine_env();
    let own = name.split_once('.').map_or(name, |(_, source)| source);
    let context = MjValue::from_pairs([
        ("env", MjValue::from(Serde(vars))),
        (
            "_",
            underscore_with(&jinja, &[("name", MjValue::from(own))]),
        ),
    ]);
    let ctx = Arc::new(EngineCtx {
        env: vars.clone(),
        allow_exec: false,
        origin: std::sync::Mutex::new(Some(format!("{name} enabled.template"))),
        ..Default::default()
    });
    let value =
        with_ctx(ctx, || eval_line_with(&jinja, template, context)).map_err(|e| e.to_string())?;
    use serde_json::Value as Json;
    Ok(match value {
        None | Some(Json::Null) => false,
        Some(Json::Bool(b)) => b,
        Some(Json::String(s)) => !s.is_empty(),
        Some(Json::Number(n)) => n.as_f64() != Some(0.0),
        Some(Json::Array(a)) => !a.is_empty(),
        Some(Json::Object(o)) => !o.is_empty(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> toml::Table {
        text.parse::<toml::Table>().unwrap()
    }

    #[test]
    fn a_gate_takes_its_own_poll_then_the_abstract_providers_then_30s() {
        let providers = table(
            "[_]\nenabled.poll = \"10s\"\n[a]\nenabled.template = \"{{ 1 }}\"\n\
             [b]\nenabled.template = \"{{ 1 }}\"\nenabled.poll = \"2s\"\n[c]\nenabled = false\n",
        );
        let found = gates(&providers);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found["a"].poll, Duration::from_secs(10));
        assert_eq!(found["b"].poll, Duration::from_secs(2));
        let bare = gates(&table("[a]\nenabled.template = \"{{ 1 }}\"\n"));
        assert_eq!(bare["a"].poll, DEFAULT_POLL);
    }

    #[test]
    fn a_sources_gate_is_named_p_dot_s_and_polls_by_its_prototypes() {
        let providers = table(
            "[_]\nenabled.poll = \"10s\"\n\
             [a.sources.x]\nenabled.template = \"{{ 1 }}\"\n\
             [a.sources.y]\nenabled.template = \"{{ 1 }}\"\nenabled.poll = \"2s\"\n\
             [a.sources.z]\nenabled = false\n\
             [b.sources._]\nenabled.poll = \"3s\"\n[b.sources.x]\nenabled.template = \"{{ 1 }}\"\n\
             [c.sources.x]\nenabled = true\n",
        );
        let found = gates(&providers);
        assert_eq!(found.len(), 3, "{found:?}");
        assert_eq!(
            found["a.x"].poll,
            Duration::from_secs(10),
            "the abstract provider's"
        );
        assert_eq!(found["a.y"].poll, Duration::from_secs(2));
        assert_eq!(found["b.x"].poll, Duration::from_secs(3), "its prototype's");
        assert_eq!(sources_off(&providers), ["a.z"]);
        assert_eq!(path_of("a.x"), "[daemon.providers.a.sources.x]");
        assert_eq!(render("a.x", "{{ _.name }}", &HashMap::new()), Ok(true));
        assert_eq!(
            render("a.x", "{{ _.name == 'x' }}", &HashMap::new()),
            Ok(true)
        );
    }

    #[test]
    fn a_check_that_runs_something_is_a_load_error_naming_its_line() {
        let probe = probe();
        for line in [
            "{{ _.exec('true') }}",
            "{{ 'true' | execa }}",
            "{{ _.url('http://x') }}",
            "{{ _.plugin('x', 'y') }}",
        ] {
            let errors = lint_gate(&probe, "docker", line);
            assert!(
                matches!(errors.as_slice(), [e] if e.starts_with("[daemon.providers.docker] enabled.template: ")
                    && e.contains("not available in a check")),
                "{line}: {errors:?}"
            );
        }
        assert!(lint_gate(&probe, "d", "{{ '/x' | read and _.now() }}").is_empty());
        assert!(!lint_gate(&probe, "d", "{{ nosuch }}").is_empty());
    }

    #[test]
    fn a_check_renders_to_a_truthiness() {
        let vars = HashMap::from([("ON".to_string(), "1".to_string())]);
        for (line, on) in [
            ("{{ none }}", false),
            ("{{ false }}", false),
            ("{{ '' }}", false),
            ("{{ [] }}", false),
            ("{{ 0 }}", false),
            ("{{ true }}", true),
            ("{{ env.ON }}", true),
            ("{{ env.OFF }}", false),
            ("{{ _.name == 'p' }}", true),
        ] {
            assert_eq!(render("p", line, &vars), Ok(on), "{line}");
        }
        assert!(render("p", "{{ _.fail('no') }}", &vars).is_err());
    }
}
