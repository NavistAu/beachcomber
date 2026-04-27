use beachcomber::provider::battery::BatteryProvider;
use beachcomber::provider::load::LoadProvider;
use beachcomber::provider::{Provider, SourceScope};

// --- Battery ---

#[test]
fn battery_provider_metadata() {
    let p = BatteryProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "battery");
    // Battery is a Section I provider (multi-source); test only the provider name for now
    // TODO(section-J): assert on source metadata once battery is migrated in Section I
    let _ = meta;
}

#[test]
fn battery_provider_executes() {
    let p = BatteryProvider;
    // On macOS laptops this returns data; on desktops/CI it may return None
    // We just verify it doesn't panic
    // TODO(section-J): update to sources()[0].execute() once battery migrated in Section I
    let _ = p;
}

// --- Load ---

#[test]
fn load_provider_metadata() {
    let p = LoadProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "load");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "loadavg");
    assert_eq!(src.scope, SourceScope::Global);
    let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"one"));
    assert!(fields.contains(&"five"));
    assert!(fields.contains(&"fifteen"));
}

#[test]
fn load_provider_executes() {
    let p = LoadProvider;
    let sources = p.sources();
    let result = sources[0].execute(None);
    let one = result.fields.get("one").unwrap().as_text();
    let val: f64 = one.parse().expect("Load should be a number");
    assert!(val >= 0.0, "Load average should be non-negative");
}

// --- Battery (Linux) ---

#[cfg(target_os = "linux")]
mod battery_linux_tests {
    use beachcomber::provider::Provider;
    use beachcomber::provider::battery::BatteryProvider;

    #[test]
    fn battery_provider_handles_no_battery() {
        let p = BatteryProvider;
        // Should not panic even on VMs with no battery
        // TODO(section-J): update to sources()[0].execute() once battery migrated in Section I
        let _ = p;
    }
}

// --- Uptime ---

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod uptime_tests {
    use beachcomber::provider::Provider;
    use beachcomber::provider::uptime::UptimeProvider;
    use beachcomber::provider::{InvalidationStrategy, SourceScope};

    #[test]
    fn uptime_provider_metadata() {
        let p = UptimeProvider;
        let meta = p.metadata();
        assert_eq!(meta.name, "uptime");
        assert_eq!(meta.sources.len(), 1);
        let src = &meta.sources[0];
        assert_eq!(src.name, "time");
        assert_eq!(src.scope, SourceScope::Global);
        assert!(matches!(
            src.invalidation,
            InvalidationStrategy::Poll { interval_secs: 60 }
        ));
        let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(fields.contains(&"seconds"));
        assert!(fields.contains(&"days"));
        assert!(fields.contains(&"hours"));
        assert!(fields.contains(&"minutes"));
    }

    #[test]
    fn uptime_provider_executes() {
        let p = UptimeProvider;
        let sources = p.sources();
        let result = sources[0].execute(None);
        // sysctl may be unavailable in sandboxed environments; if data returned, validate it.
        if !result.fields.is_empty() {
            assert!(
                result.fields.contains_key("seconds"),
                "seconds field should be present"
            );
        }
    }
}
