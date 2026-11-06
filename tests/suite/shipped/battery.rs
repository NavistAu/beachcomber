//! `defaults/battery.toml`: the battery, from `sys`.

use super::scenario;
use serde_json::json;

/// A machine with no battery has no fields (a miss); one with a battery has
/// a percent and whether it charges.
#[test]
fn battery_provider_executes() {
    let s = scenario();
    let Some(battery) = s.get("battery", &s.project) else {
        return;
    };
    assert!(
        battery["percent"]
            .as_i64()
            .is_some_and(|p| (0..=100).contains(&p)),
        "{battery}"
    );
    assert!(battery["charging"].is_boolean(), "{battery}");
    for field in ["status", "status_raw", "time_remaining_secs"] {
        assert_ne!(battery.get(field), Some(&json!("")), "{field}: {battery}");
    }
}

/// The deliberate difference, pinned on every platform: Linux's battery is
/// one Source, `state`, from sysfs every 30 seconds, where it was `level`
/// (sysfs) beside `upower` (two `upower` spawns every minute). It declares
/// `status_raw` everywhere; only Linux's kernel gives it a value.
#[test]
fn battery_is_one_source_on_every_platform() {
    let registry = crate::common::registry::with_shipped_defaults();
    let sources = registry.provider_sources("battery").expect("battery");
    assert_eq!(sources.len(), 1, "{sources:?}");
    let state = &sources[0];
    assert_eq!(state.name, "state");
    let mut fields: Vec<&str> = state.fields.iter().map(|f| f.name.as_str()).collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "charging",
            "percent",
            "status",
            "status_raw",
            "time_remaining_secs"
        ]
    );
    assert_eq!(
        state.invalidation,
        beachcomber::provider::InvalidationStrategy::Poll {
            interval_secs: 30,
            count: 4
        }
    );
}
