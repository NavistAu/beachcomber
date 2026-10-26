//! `defaults/load.toml`: the load averages, from `sys`.

use super::scenario;
use serde_json::json;

#[test]
fn load_provider_executes() {
    let s = scenario();
    let load = s.get("load", &s.project).expect("load answers");
    for field in ["one", "five", "fifteen"] {
        assert!(
            load[field].as_f64().is_some_and(|v| v >= 0.0),
            "{field} is a number: {load}"
        );
    }
    let leaf = s.get("load:cache.one", &s.project).expect("a cache leaf");
    assert_eq!(
        leaf["poll"],
        json!({"interval": "10s", "count": 6}),
        "{leaf}"
    );
}
