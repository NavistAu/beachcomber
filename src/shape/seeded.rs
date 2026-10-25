//! What one configuration file says in the two roots the daemon seeds from.
//!
//! Seeding is per file, so the rules are checked per file too: [`read`] is the
//! one typed parse of a single layer, and returns both the errors in it and
//! the writes it makes. Startup, `comb check config`, reload and the seeding
//! itself all go through it, so a file is accepted exactly when what it seeds
//! was checked, and nothing is written that did not validate.
//!
//! - `[client.providers.<p>] template` is fenced here: a tree of tables whose
//!   string leaves are value expressions. What it seeds is the provider's
//!   `template` line's to say ([`crate::shape::provider`], run by the seeding
//!   itself over the layer's `[client]` tree), so a file is checked here and
//!   nothing is written from it.
//! - `[daemon.providers.<p>.cache._] decay` seeds `<p>:cache`, an integer from
//!   1 to [`MAX_DECAY`]; the abstract provider `_` seeds `*:cache`, the
//!   prototype every provider without its own falls back to.
//!
//! `[daemon.providers.<p>]` also takes the provider's own lines, `template`,
//! `result` and `return`, each a jinja line (what they mean, and the load
//! errors that need the composed tree, are [`crate::shape::provider`]'s).
//! `enabled = false` is a bool on a named provider: sugar for the empty
//! `result`, so a provider that produces nothing and runs no Source.
//! `[daemon.providers.<p>.sources.<s>]` is fenced here too (`root`, `data`,
//! `result`, `return`, each a jinja line); what a Source means is
//! [`crate::shape::build`]'s, from the composed tree.
//!
//! `[daemon.plugin] path` is checked here too, though nothing is seeded from it:
//! a list of directory strings, each absolute or under `~`.
//!
//! So is the rest of the file's shape: the closed list of bare `[daemon]` keys
//! with their types (`budget.execute` among them), and the two roots themselves.
//! A top-level table other than `client` and `daemon` is an error, naming where
//! its contents went when it held something.
//!
//! Every error is prefixed with the file's `config:<path>` label.

use crate::config::{Config, LOG_LEVELS, log_level_filter, parse_duration, parse_whole_seconds};
use crate::scheduler::lifecycle::{MAX_DECAY, decay_error};
use std::path::Path;

/// What one layer seeds, and what is wrong with it.
#[derive(Debug, Default)]
pub struct Seeded {
    /// Each prefixed with the file's `config:<path>` label.
    pub errors: Vec<String>,
    /// `(namespace, tree)`: only trees that validated.
    pub writes: Vec<(String, serde_json::Value)>,
    /// The providers whose `[client.providers.<p>] template` failed the fence
    /// above: already reported, so the `template` line is not run over it.
    pub fenced: std::collections::BTreeSet<String>,
}

/// A provider name as a config key, or the reason it is not one. `client`
/// says which root: `_`, the prototype's name, is a provider only in the
/// daemon root's `cache` prototype and never in the client's.
pub fn provider_name_error(root: &str, name: &str, client: bool) -> Option<String> {
    if name.is_empty() {
        return Some(format!("empty provider name in [{root}]"));
    }
    if name == "*" {
        let hint = if client {
            ""
        } else {
            "; '_' names every provider"
        };
        return Some(format!(
            "invalid provider name '*' in [{root}]: '*' is not a provider name{hint}"
        ));
    }
    // A name is a key's head (`libbeachcomber::key`): it ends at the first
    // `.` or `[`, holds a `:` only before a facet, and is never a directory.
    if name.contains([':', '.', '[', ']']) || name.starts_with(['/', '~']) {
        return Some(format!(
            "invalid provider name '{name}' in [{root}]: a name holds no ':', '.', '[' or ']', \
             and does not start with '/' or '~'"
        ));
    }
    if name == "_" && client {
        return Some(format!(
            "invalid provider name '_' in [{root}]: '_' is the prototype name, not a provider"
        ));
    }
    None
}

struct Reader {
    label: String,
    out: Seeded,
}

impl Reader {
    fn err(&mut self, message: String) {
        self.out.errors.push(format!("{}: {message}", self.label));
    }

    fn client(&mut self, client: &toml::Value) {
        let Some(table) = client.as_table() else {
            return self.err("[client] must be a table".into());
        };
        for (key, value) in table {
            if key != "providers" {
                self.err(format!("unknown key '{key}' in [client]"));
                continue;
            }
            let Some(providers) = value.as_table() else {
                self.err("[client.providers] must be a table".into());
                continue;
            };
            for (name, provider) in providers {
                match provider_name_error("client.providers", name, true) {
                    Some(e) => self.err(e),
                    None => self.client_provider(name, provider),
                }
            }
        }
    }

    fn client_provider(&mut self, name: &str, value: &toml::Value) {
        let Some(table) = value.as_table() else {
            return self.err(format!("[client.providers.{name}] must be a table"));
        };
        for (key, sub) in table {
            if key != "template" {
                self.err(format!("unknown key '{key}' in [client.providers.{name}]"));
                continue;
            }
            let Some(tree) = sub.as_table() else {
                self.err(format!(
                    "[client.providers.{name}] template must be a table of value expressions"
                ));
                continue;
            };
            let before = self.out.errors.len();
            self.template_tree(name, "", tree);
            if self.out.errors.len() > before {
                self.out.fenced.insert(name.to_string());
            }
        }
    }

    /// Walk a template's tree. A dotted key is an error: nesting is tables, so
    /// `"a.b"` names one record while addressing another, and the cache
    /// refuses the write anyway. So is an empty key, and a leaf that is
    /// neither a string nor a table (not a value expression).
    fn template_tree(&mut self, provider: &str, prefix: &str, table: &toml::Table) {
        for (key, value) in table {
            let addressed = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            if key.is_empty() {
                let at = if prefix.is_empty() {
                    "template".to_string()
                } else {
                    format!("template.{prefix}")
                };
                self.err(format!(
                    "[client.providers.{provider}] {at} has an empty key"
                ));
                continue;
            }
            match value {
                toml::Value::Table(sub) => self.template_tree(provider, &addressed, sub),
                toml::Value::String(_) => {}
                other => self.err(format!(
                    "[client.providers.{provider}] template '{addressed}' is {}, not a value \
                     expression or a group.",
                    other.type_str()
                )),
            }
        }
    }

    /// `[daemon] plugin`: a table holding only `path`, a list of non-empty
    /// directory strings, each absolute or beginning `~/`.
    fn daemon_plugin(&mut self, plugin: &toml::Value) {
        let Some(table) = plugin.as_table() else {
            return self.err("[daemon] plugin must be a table".into());
        };
        for (key, value) in table {
            if key != "path" {
                self.err(format!("unknown key '{key}' in [daemon.plugin]"));
                continue;
            }
            let dirs: Option<Vec<&str>> = value
                .as_array()
                .and_then(|list| list.iter().map(toml::Value::as_str).collect());
            let Some(dirs) = dirs else {
                self.err("[daemon.plugin] path must be a list of directory strings".into());
                continue;
            };
            for dir in dirs {
                if dir.is_empty() {
                    self.err("[daemon.plugin] path has an empty entry".into());
                } else if !(dir.starts_with('/') || dir == "~" || dir.starts_with("~/")) {
                    self.err(format!(
                        "[daemon.plugin] path entry '{dir}' must be an absolute directory or \
                         start with ~/"
                    ));
                }
            }
        }
    }

    /// `[daemon]`'s bare keys: the list is closed, and each value has its type
    /// and range checked here, so a mistyped one is an error in the file that
    /// wrote it and never a silent fall back to the default configuration.
    /// `providers` and `plugin` are read by their own checks.
    fn daemon_bare(&mut self, daemon: &toml::Value) {
        let Some(table) = daemon.as_table() else {
            return self.err("[daemon] must be a table".into());
        };
        for (key, value) in table {
            match key.as_str() {
                "providers" | "plugin" => {}
                "socket_path" | "env_file" => match value.as_str() {
                    Some("") => self.err(format!("[daemon] {key} must not be empty")),
                    Some(_) => {}
                    None => self.err(format!(
                        "[daemon] {key} must be a string, not {}",
                        value.type_str()
                    )),
                },
                "log_level" => {
                    if value.as_str().and_then(log_level_filter).is_none() {
                        self.err(format!(
                            "[daemon] log_level must be one of {}, got {value}",
                            LOG_LEVELS.join(", ")
                        ));
                    }
                }
                "provider_timeout_secs" | "idle_shutdown_secs" => {
                    if value.as_integer().is_none_or(|n| n < 1) {
                        self.err(format!(
                            "[daemon] {key} must be an integer of at least 1 (seconds), got {value}"
                        ));
                    }
                }
                "watchdog_interval" | "watchdog_threshold" => {
                    let whole = value
                        .as_str()
                        .and_then(parse_whole_seconds)
                        .filter(|d| !d.is_zero());
                    if whole.is_none() {
                        self.err(format!(
                            "[daemon] {key} must be a duration of whole seconds, at least 1s \
                             (\"30s\", \"2m\"), got {value}"
                        ));
                    }
                }
                "budget" => self.daemon_budget(value),
                _ => self.err(format!("unknown key '{key}' in [daemon]")),
            }
        }
    }

    /// `[daemon] budget`: a table holding only `execute`, a duration of at
    /// least 1ms (`"50ms"`, `"2s"`).
    fn daemon_budget(&mut self, budget: &toml::Value) {
        let Some(table) = budget.as_table() else {
            return self.err("[daemon] budget must be a table".into());
        };
        for (key, value) in table {
            if key != "execute" {
                self.err(format!("unknown key '{key}' in [daemon.budget]"));
            } else if value.as_str().and_then(parse_duration).is_none() {
                self.err(format!(
                    "[daemon.budget] execute must be a duration of at least 1ms (\"50ms\", \
                     \"2s\"), got {value}"
                ));
            }
        }
    }

    /// A top-level table other than `client` and `daemon`: removed or never
    /// existed. The ones that held something say where it went.
    fn top_level(&mut self, key: &str, value: &toml::Value) {
        let moved = match key {
            "providers" => {
                ": a provider's settings are [daemon.providers.<p>] (the daemon's) and \
                 [client.providers.<p>] (the client's); a built-in Source is retuned by \
                 [daemon.providers.<p>.sources.<s>] return; templates live in \
                 [client.providers.<p>] template.<leaf>, decay in \
                 [daemon.providers.<p>.cache._]"
            }
            "lifecycle" => {
                ": idle_shutdown_secs is [daemon] idle_shutdown_secs; a Source sets its \
                 schedule on its return line"
            }
            "failback" => ": a Source sets failback on its return line",
            "tree" => ": removed, nothing reads it",
            _ => "",
        };
        if value.is_table() {
            self.err(format!("unknown table [{key}]{moved}"));
        } else {
            self.err(format!("unknown key '{key}' at the top level{moved}"));
        }
    }

    fn daemon_providers(&mut self, providers: &toml::Value) {
        let Some(providers) = providers.as_table() else {
            return self.err("[daemon.providers] must be a table".into());
        };
        for (name, provider) in providers {
            match provider_name_error("daemon.providers", name, false) {
                Some(e) => self.err(e),
                None => self.daemon_provider(name, provider),
            }
        }
    }

    /// `[daemon.providers.<p>]` holds `cache` (which holds only `_`, which
    /// holds only `decay`), `sources`, the provider's lines: `template`,
    /// `result` and `return`, each a string, and `enabled`, a bool (not on
    /// `_`: it would switch off every provider).
    fn daemon_provider(&mut self, name: &str, value: &toml::Value) {
        let Some(table) = value.as_table() else {
            return self.err(format!("[daemon.providers.{name}] must be a table"));
        };
        let namespace = if name == "_" {
            "*:cache".to_string()
        } else {
            format!("{name}:cache")
        };
        for (key, sub) in table {
            if key == "sources" {
                self.sources(name, sub);
                continue;
            }
            if key == "enabled" {
                if name == "_" {
                    self.err(
                        "[daemon.providers._] enabled: the abstract provider applies to every \
                         provider: name the provider, [daemon.providers.<p>] enabled = false"
                            .into(),
                    );
                } else if !sub.is_bool() {
                    self.err(format!(
                        "[daemon.providers.{name}] enabled must be true or false, not {}",
                        sub.type_str()
                    ));
                }
                continue;
            }
            if matches!(key.as_str(), "template" | "result" | "return") {
                if !sub.is_str() {
                    self.err(format!(
                        "[daemon.providers.{name}] {key} must be a string (a jinja line), not {}",
                        sub.type_str()
                    ));
                }
                continue;
            }
            if key != "cache" {
                self.err(format!("unknown key '{key}' in [daemon.providers.{name}]"));
                continue;
            }
            let Some(cache) = sub.as_table() else {
                self.err(format!("[daemon.providers.{name}.cache] must be a table"));
                continue;
            };
            for (key, sub) in cache {
                if key != "_" {
                    self.err(format!(
                        "unknown key '{key}' in [daemon.providers.{name}.cache]"
                    ));
                    continue;
                }
                let Some(proto) = sub.as_table() else {
                    self.err(format!("[daemon.providers.{name}.cache._] must be a table"));
                    continue;
                };
                for (key, sub) in proto {
                    if key != "decay" {
                        self.err(format!(
                            "unknown key '{key}' in [daemon.providers.{name}.cache._]"
                        ));
                    } else if let toml::Value::Integer(n) = sub
                        && (1..=MAX_DECAY).contains(n)
                    {
                        self.out
                            .writes
                            .push((namespace.clone(), serde_json::json!({"_": {"decay": n}})));
                    } else {
                        self.err(decay_error(
                            &format!("{namespace}._.decay"),
                            &sub.to_string(),
                        ));
                    }
                }
            }
        }
    }
}

impl Reader {
    /// `[daemon.providers.<p>.sources]`: a table of Source tables, `_` the
    /// prototype. The abstract provider has only the abstract Source.
    fn sources(&mut self, provider: &str, value: &toml::Value) {
        let at = format!("[daemon.providers.{provider}.sources]");
        let Some(sources) = value.as_table() else {
            return self.err(format!("{at} must be a table"));
        };
        for (name, source) in sources {
            if provider == "_" && name != "_" {
                self.err(format!(
                    "{at} holds only '_' (the abstract Source): '{name}' would be a Source of \
                     every provider"
                ));
                continue;
            }
            if name.is_empty() || name.contains(['.', ':', '*']) || name.starts_with('<') {
                self.err(format!(
                    "invalid Source name '{name}' in {at}: a name is non-empty, holds no '.', \
                     ':' or '*' and does not start with '<'"
                ));
                continue;
            }
            self.source(provider, name, source);
        }
    }

    /// `[daemon.providers.<p>.sources.<s>]` holds `root`, `data`, `result`
    /// (a line, or a table of field lines) and `return`; each a string.
    fn source(&mut self, provider: &str, name: &str, value: &toml::Value) {
        let at = format!("[daemon.providers.{provider}.sources.{name}]");
        let Some(table) = value.as_table() else {
            return self.err(format!("{at} must be a table"));
        };
        for (key, line) in table {
            match (key.as_str(), line) {
                ("root" | "data" | "return", toml::Value::String(_)) => {}
                ("result", toml::Value::String(_)) => {}
                ("result", toml::Value::Table(fields)) => {
                    for (field, line) in fields {
                        if !line.is_str() {
                            self.err(format!(
                                "{at} result.{field} must be a string (a jinja line), not {}",
                                line.type_str()
                            ));
                        }
                    }
                }
                ("root" | "data" | "return", other) => self.err(format!(
                    "{at} {key} must be a string (a jinja line), not {}",
                    other.type_str()
                )),
                ("result", other) => self.err(format!(
                    "{at} result must be a string or a table of field lines, not {}",
                    other.type_str()
                )),
                _ => self.err(format!("unknown key '{key}' in {at}")),
            }
        }
    }
}

/// Read one layer: the file it came from (for the label) and its parsed TOML.
pub fn read(file: &Path, layer: &toml::Value) -> Seeded {
    read_labelled(Config::config_label(file), layer)
}

/// [`read`] for a layer with a label of its own: a configuration built in
/// memory corresponds to no file.
pub fn read_labelled(label: String, layer: &toml::Value) -> Seeded {
    let mut r = Reader {
        label,
        out: Seeded::default(),
    };
    let Some(top) = layer.as_table() else {
        r.err("top-level value must be a table".into());
        return r.out;
    };
    for (key, value) in top {
        match key.as_str() {
            "client" => r.client(value),
            "daemon" => {
                r.daemon_bare(value);
                if let Some(providers) = value.get("providers") {
                    r.daemon_providers(providers);
                }
                if let Some(plugin) = value.get("plugin") {
                    r.daemon_plugin(plugin);
                }
            }
            other => r.top_level(other, value),
        }
    }
    r.out
}
