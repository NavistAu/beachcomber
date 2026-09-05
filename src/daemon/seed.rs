//! Seeding facet namespaces from configuration (spec §3.1, §3.2, §3.9).
//!
//! Every `[providers."<p>:<facet>"]` table in configuration is written into the
//! cache at namespace `<p>:<facet>` and the global path, each record labelled
//! `config:<file>` with the file that set it. Layers are written in composition
//! order — the main config file, then `conf.d/*.toml` lexically — one
//! `write_static` per layer with `replace = false`. A record two files set ends
//! up labelled by the later one (`Cache::write_static` moves a record out of
//! whichever static map held it), and a record only the earlier file set keeps
//! its own label.
//!
//! Settings still compose by deep merge. Facet tables deliberately do not: the
//! merge would collapse the layers into one value and the file a record came
//! from — the thing `comb status` shows as its source — would be gone before it
//! reached the cache.

use crate::cache::{CONFIG_SOURCE_PREFIX, Cache};
use crate::config::Config;
use crate::provider::Value;
use crate::provider::registry::ProviderRegistry;
use std::path::PathBuf;

/// Write every facet table of every layer into the cache. Returns the warnings
/// the caller should log — one per namespace whose write was refused.
pub fn seed_facets(
    cache: &Cache,
    registry: &ProviderRegistry,
    layers: &[(PathBuf, toml::Value)],
) -> Vec<String> {
    let mut warnings = Vec::new();
    for (file, layer) in layers {
        let label = Config::config_label(file);
        for (namespace, table) in Config::facet_tables(layer) {
            registry.register_static(&namespace);
            let is_template =
                crate::query::parse_facet(&namespace).ok().flatten() == Some("template");
            let value = Value::from_json(&facet_json(&table, is_template));
            if let Err(e) =
                cache.write_static(&namespace, None, &label, "", value, false, None, registry)
            {
                warnings.push(format!("{label}: could not seed {namespace}: {e}"));
            }
        }
    }
    warnings
}

/// Re-seed in place, with no restart: every `config:` record is dropped, then
/// all layers are written again.
///
/// The consequences, in the order an operator meets them: a leaf edited in a
/// file takes effect on the next read; a leaf deleted from a file disappears; a
/// leaf a later file overrides stays overridden; and a `put` record survives,
/// including one that shadows a config record since deleted from its file,
/// because only `config:` labels are dropped. A `watch` wakes either way —
/// `Cache::drop_static_records_with_label_prefix` notifies the entries it
/// empties and `write_static` notifies the ones it fills.
pub fn reseed_facets(
    cache: &Cache,
    registry: &ProviderRegistry,
    layers: &[(PathBuf, toml::Value)],
) -> Vec<String> {
    cache.drop_static_records_with_label_prefix(CONFIG_SOURCE_PREFIX);
    seed_facets(cache, registry, layers)
}

/// One facet table as the JSON tree the cache stores: the TOML maps 1:1, with
/// nested tables becoming objects.
///
/// Under `template` a leaf that is neither a string nor a table is not a value
/// expression, so it is dropped — `Config::validate_providers` has already
/// warned about it by name.
fn facet_json(table: &toml::Table, is_template: bool) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for (key, value) in table {
        let converted = match value {
            toml::Value::Table(sub) => facet_json(sub, is_template),
            toml::Value::String(s) => serde_json::Value::String(s.clone()),
            _ if is_template => continue,
            other => serde_json::to_value(other).unwrap_or(serde_json::Value::Null),
        };
        out.insert(key.clone(), converted);
    }
    serde_json::Value::Object(out)
}
