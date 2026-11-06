//! `comb check config --origins`: how the composed configuration came
//! together, after `git config --list --show-origin` and
//! `systemd-analyze cat-config`.
//!
//! Every leaf of the composition, its effective value, the file that set it,
//! and every earlier file that set the same leaf and was overridden. Read from
//! the files the way [`Config::load`] composes them, so it needs no daemon and
//! works when the daemon cannot start.

use crate::cli::status_format::{bold, dim, sanitize_cell, tree_branch};
use crate::config::Config;
use libbeachcomber::term;
use std::path::{Path, PathBuf};

/// One leaf of the composed configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Origin {
    /// The leaf's TOML key path, in canonical key form
    /// (`daemon.providers.git.sources._.return`, `template['a.b']`).
    pub key: String,
    /// What the composition holds there.
    pub value: toml::Value,
    /// The file that set it; `None` for comb's own default, which no file sets.
    pub file: Option<PathBuf>,
    /// Every earlier file that set the same leaf, the one it overrode first:
    /// `conf.d/` lexically last to first, then `config.toml`, then
    /// `defaults/`.
    pub shadowed: Vec<(PathBuf, toml::Value)>,
}

/// Every leaf of `config`'s composition ([`Config::composed`]), sorted by
/// key, with comb's defaults for the settings no file sets: what
/// [`Config::applied`] holds that the composition does not, so the view cannot
/// claim a default comb does not use, nor leave out one it does.
pub fn origins(config: &Config) -> Vec<Origin> {
    let layers = config.composed_layers();
    let mut leaves = Vec::new();
    collect_leaves(config.composed(), &mut Vec::new(), &mut leaves);

    let mut out: Vec<(Vec<String>, Origin)> = leaves
        .into_iter()
        .map(|(segments, value)| {
            // A file set a leaf where its own value sits at that path; the
            // last one to is the one that holds, the merge being last-wins.
            let mut setters: Vec<(PathBuf, toml::Value)> = layers
                .iter()
                .filter_map(|(file, layer)| {
                    at_path(layer, &segments).map(|v| (file.clone(), v.clone()))
                })
                .collect();
            let file = setters.pop().map(|(file, _)| file);
            setters.reverse();
            let origin = Origin {
                key: libbeachcomber::key::format_address(&segments),
                value,
                file,
                shadowed: setters,
            };
            (segments, origin)
        })
        .collect();

    let mut defaults = Vec::new();
    collect_leaves(&config.applied(), &mut Vec::new(), &mut defaults);
    for (segments, value) in defaults {
        if at_path(config.composed(), &segments).is_none() {
            out.push((
                segments.clone(),
                Origin {
                    key: libbeachcomber::key::format_address(&segments),
                    value,
                    file: None,
                    shadowed: Vec::new(),
                },
            ));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.into_iter().map(|(_, origin)| origin).collect()
}

/// The leaves of `value`: everything that is not a table with keys in it. An
/// empty table is a leaf, since naming one is a setting of its own
/// (`[client.providers.demo]` names a provider).
fn collect_leaves(
    value: &toml::Value,
    at: &mut Vec<String>,
    out: &mut Vec<(Vec<String>, toml::Value)>,
) {
    match value {
        toml::Value::Table(table) if !table.is_empty() => {
            for (key, sub) in table {
                at.push(key.clone());
                collect_leaves(sub, at, out);
                at.pop();
            }
        }
        _ if !at.is_empty() => out.push((at.clone(), value.clone())),
        _ => {}
    }
}

/// The value `layer` holds at `segments`, walking tables only.
fn at_path<'a>(layer: &'a toml::Value, segments: &[String]) -> Option<&'a toml::Value> {
    segments
        .iter()
        .try_fold(layer, |node, segment| node.as_table()?.get(segment))
}

/// Whether `key` is `filter` or lies below it, segment by segment.
fn under(key_segments: &[String], filter: &[String]) -> bool {
    key_segments.starts_with(filter)
}

/// The origins below `filter`, a key in canonical form; an empty filter is
/// everything. `Err` for a filter that is not a key.
pub fn select(origins: Vec<Origin>, filter: &str) -> Result<Vec<Origin>, String> {
    if filter.is_empty() {
        return Ok(origins);
    }
    let want = libbeachcomber::key::parse_address(filter)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("invalid key: {filter}"))?;
    Ok(origins
        .into_iter()
        .filter(|o| {
            libbeachcomber::key::parse_address(&o.key)
                .is_some_and(|segments| under(&segments, &want))
        })
        .collect())
}

/// A value as a cell: a string as itself, anything else as TOML writes it.
fn value_text(value: &toml::Value, ascii: bool) -> String {
    let text = match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Table(t) if t.is_empty() => "{}".to_string(),
        other => other.to_string(),
    };
    sanitize_cell(&text, ascii)
}

/// Where a leaf came from, as `status` labels a config-seeded record:
/// `config(<file>)`, `$HOME` written as `~`; comb's own value as `built-in`.
fn source_text(file: Option<&Path>) -> String {
    let Some(file) = file else {
        return "built-in".to_string();
    };
    format!(
        "config({})",
        libbeachcomber::path::Home::from_env().compact_path(file)
    )
}

/// The human table: KEY, SOURCE, VALUE, each leaf followed by what it shadowed,
/// hanging under it as `status --tree` hangs a value's layers. VALUE is last
/// and never cut: a template line is as long as it is.
pub fn render_human(origins: &[Origin], color: bool, ascii: bool) -> String {
    let mut rows: Vec<(String, String, String, bool)> = Vec::new();
    for o in origins {
        rows.push((
            o.key.clone(),
            source_text(o.file.as_deref()),
            value_text(&o.value, ascii),
            false,
        ));
        for (i, (file, value)) in o.shadowed.iter().enumerate() {
            let last = i + 1 == o.shadowed.len();
            rows.push((
                format!("{} shadowed", tree_branch(last, ascii)),
                source_text(Some(file)),
                value_text(value, ascii),
                true,
            ));
        }
    }
    let width = |pick: fn(&(String, String, String, bool)) -> &String, header: &str| {
        rows.iter()
            .map(|r| term::width(pick(r)))
            .chain(std::iter::once(header.len()))
            .max()
            .unwrap_or(0)
    };
    let key_w = width(|r| &r.0, "KEY");
    let source_w = width(|r| &r.1, "SOURCE");
    let pad = crate::cli::status_format::pad_cols;

    let mut out = String::new();
    let header = format!(
        "  {}  {}  VALUE",
        pad("KEY", key_w),
        pad("SOURCE", source_w)
    );
    out.push_str(&bold(&header, color));
    out.push('\n');
    for (key, source, value, shadowed) in rows {
        let line = format!(
            "  {}  {}  {value}",
            pad(&key, key_w),
            pad(&source, source_w)
        );
        if shadowed {
            out.push_str(&dim(&line, color));
        } else {
            out.push_str(&line);
        }
        out.push('\n');
    }
    out
}

/// One JSON object per leaf: `{key, value, file, shadowed: [{file, value}]}`,
/// `file` the absolute path, or `null` for comb's own built-in value.
pub fn render_json(origins: &[Origin]) -> String {
    let mut out = String::new();
    for o in origins {
        let row = serde_json::json!({
            "key": o.key,
            "value": toml_to_json(&o.value),
            "file": o.file.as_ref().map(|f| f.display().to_string()),
            "shadowed": o.shadowed.iter().map(|(file, value)| serde_json::json!({
                "file": file.display().to_string(),
                "value": toml_to_json(value),
            })).collect::<Vec<_>>(),
        });
        out.push_str(&row.to_string());
        out.push('\n');
    }
    out
}

/// A TOML value as JSON; a datetime as the text TOML writes it.
fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::Value::from(*i),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
        toml::Value::Array(a) => serde_json::Value::Array(a.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => serde_json::Value::Object(
            t.iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(text: &str) -> toml::Value {
        toml::from_str(text).unwrap()
    }

    fn config(layers: &[(&str, &str)]) -> Config {
        Config::from_layers(
            layers
                .iter()
                .map(|(f, t)| (PathBuf::from(f), layer(t)))
                .collect(),
        )
    }

    fn find<'a>(origins: &'a [Origin], key: &str) -> &'a Origin {
        origins
            .iter()
            .find(|o| o.key == key)
            .unwrap_or_else(|| panic!("{key} in {origins:#?}"))
    }

    /// SOURCE is padded in terminal columns, so a wide path does not push the
    /// VALUE column out of line.
    #[test]
    fn a_wide_value_aligns() {
        let c = config(&[
            ("/日本語/a.toml", "[daemon]\nlog_level = \"warn\"\n"),
            ("/b.toml", "[daemon]\nidle_shutdown_secs = 77\n"),
        ]);
        let out = render_human(&origins(&c), false, false);
        let column = |token: &str| {
            let line = out
                .lines()
                .find(|l| l.contains(token))
                .unwrap_or_else(|| panic!("{token} in\n{out}"));
            term::width(&line[..line.rfind(token).unwrap()])
        };
        assert_eq!(column("warn"), column("77"), "{out}");
        assert!(out.contains("日本語"), "{out}");
    }

    #[test]
    fn the_last_file_to_set_a_leaf_holds_it_and_the_others_shadow_nearest_first() {
        let c = config(&[
            ("/d.toml", "[daemon]\nlog_level = \"warn\"\n"),
            ("/c.toml", "[daemon]\nlog_level = \"error\"\n"),
            ("/a.toml", "[daemon]\nlog_level = \"debug\"\n"),
        ]);
        let o = find(&origins(&c), "daemon.log_level").clone();
        assert_eq!(o.value, toml::Value::String("debug".into()));
        assert_eq!(o.file, Some(PathBuf::from("/a.toml")));
        assert_eq!(
            o.shadowed,
            vec![
                (
                    PathBuf::from("/c.toml"),
                    toml::Value::String("error".into())
                ),
                (PathBuf::from("/d.toml"), toml::Value::String("warn".into())),
            ]
        );
    }

    #[test]
    fn a_table_merges_key_by_key_so_each_key_keeps_its_own_file() {
        let c = config(&[
            (
                "/d.toml",
                "[client.providers.p]\ntemplate.a = \"1\"\ntemplate.b = \"2\"\n",
            ),
            ("/c.toml", "[client.providers.p]\ntemplate.b = \"3\"\n"),
        ]);
        let all = origins(&c);
        assert_eq!(
            find(&all, "client.providers.p.template.a").file,
            Some(PathBuf::from("/d.toml"))
        );
        assert_eq!(
            find(&all, "client.providers.p.template.b").file,
            Some(PathBuf::from("/c.toml"))
        );
    }

    #[test]
    fn a_scalar_that_replaced_a_table_shadows_it() {
        let c = config(&[
            ("/d.toml", "[daemon.budget]\nexecute = \"1s\"\n"),
            ("/c.toml", "[daemon]\nbudget = \"x\"\n"),
        ]);
        let o = find(&origins(&c), "daemon.budget").clone();
        assert_eq!(o.file, Some(PathBuf::from("/c.toml")));
        assert_eq!(o.shadowed.len(), 1, "{o:#?}");
    }

    #[test]
    fn a_default_is_listed_only_where_no_file_sets_the_key() {
        let all = origins(&config(&[]));
        let level = find(&all, "daemon.log_level");
        assert_eq!(level.file, None);
        assert_eq!(level.value, toml::Value::String("info".into()));
        assert_eq!(
            find(&all, "daemon.timeout").value,
            toml::Value::String("10s".into())
        );
        assert_eq!(
            find(&all, "daemon.budget").value,
            toml::Value::String("50ms".into())
        );

        let all = origins(&config(&[("/c.toml", "[daemon]\nlog_level = \"debug\"\n")]));
        assert_eq!(
            all.iter().filter(|o| o.key == "daemon.log_level").count(),
            1,
            "{all:#?}"
        );
    }

    /// What comb applies where no file says is what the code that applies it
    /// answers: a value worked out from another (the watchdog threshold, three
    /// intervals) and one beside the config file (the env file) are listed
    /// as the defaults they are.
    #[test]
    fn a_default_is_what_comb_applies_however_it_is_worked_out() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("config.toml");
        std::fs::write(&main, "[daemon]\nwatchdog_interval = \"30s\"\n").unwrap();
        let all = origins(&Config::load_from(&main));
        let threshold = find(&all, "daemon.watchdog_threshold");
        assert_eq!(threshold.file, None);
        assert_eq!(threshold.value, toml::Value::String("1m30s".into()));
        assert_eq!(
            find(&all, "daemon.env_file").value,
            toml::Value::String(tmp.path().join("env").display().to_string())
        );

        // A configuration with no file behind it reads no env file, and has
        // no watchdog to default the threshold of.
        let all = origins(&config(&[]));
        for key in ["daemon.env_file", "daemon.watchdog_threshold"] {
            assert!(all.iter().all(|o| o.key != key), "{key} in {all:#?}");
        }
    }

    #[test]
    fn select_takes_a_subtree_by_its_segments_not_its_text() {
        let c = config(&[(
            "/c.toml",
            "[client.providers.p]\ntemplate.a = \"1\"\n[client.providers.pq]\ntemplate.b = \"2\"\n",
        )]);
        let keys: Vec<String> = select(origins(&c), "client.providers.p")
            .unwrap()
            .into_iter()
            .map(|o| o.key)
            .collect();
        assert_eq!(keys, vec!["client.providers.p.template.a"]);
        assert!(select(origins(&c), "p['a").is_err());
    }
}
