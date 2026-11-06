//! `defaults/uptime.toml`: the time since boot, from `sys`.

use super::scenario;
use serde_json::json;

/// Needs an unsandboxed host (`sysctl`, `/proc`), as `sys`'s own
/// `uptime_provider_executes` does: CI's filter on that name matches this one.
#[test]
fn uptime_provider_executes_and_splits_its_seconds() {
    let s = scenario();
    let up = s.get("uptime", &s.project).expect("uptime answers");
    let seconds = up["seconds"].as_i64().expect("whole seconds");
    assert!(seconds > 0, "{up}");
    assert_eq!(
        (&up["days"], &up["hours"], &up["minutes"]),
        (
            &json!(seconds / 86400),
            &json!(seconds % 86400 / 3600),
            &json!(seconds % 3600 / 60)
        ),
        "{up}"
    );
    let leaf = s
        .get("uptime:cache.seconds", &s.project)
        .expect("a cache leaf");
    assert_eq!(
        leaf["poll"],
        json!({"interval": "60s", "count": 2}),
        "{leaf}"
    );
}
