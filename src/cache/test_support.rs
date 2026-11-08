//! Helpers the cache modules' tests share.

use super::*;
use crate::provider::Value;
use std::collections::HashMap;

pub(super) fn make_fields(pairs: &[(&str, &str)]) -> HashMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
        .collect()
}

pub(super) fn json(v: &Value) -> serde_json::Value {
    serde_json::to_value(v).unwrap()
}

pub(super) fn stored(cache: &Cache, ns: &str, path: Option<&str>) -> Option<CacheEntry> {
    cache.get_entry(&facet_of(ns, Facet::Cache), path)
}
