use beachcomber::provider::battery::BatteryProvider;
use beachcomber::provider::load::LoadProvider;
use beachcomber::provider::{FieldScope, InvalidationStrategy, Provider};

// --- Battery ---

#[test]
fn battery_provider_metadata() {
    let p = BatteryProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "battery");
    assert_eq!(meta.inferred_scope(), FieldScope::Global);
    let fields: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"percent"));
    assert!(fields.contains(&"charging"));
    assert!(fields.contains(&"time_remaining_secs"));
    assert!(fields.contains(&"status"));
    match meta.invalidation {
        InvalidationStrategy::Poll {
            interval_secs,
            floor_secs,
        } => {
            assert_eq!(interval_secs, 30);
            assert_eq!(floor_secs, 5);
        }
        _ => panic!("Expected Poll invalidation"),
    }
}

#[test]
fn battery_provider_executes() {
    let p = BatteryProvider;
    // On macOS laptops this returns data; on desktops/CI it may return None
    // We just verify it doesn't panic
    let _ = p.execute(None);
}

// --- Load ---

#[test]
fn load_provider_metadata() {
    let p = LoadProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "load");
    assert_eq!(meta.inferred_scope(), FieldScope::Global);
    let fields: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"one"));
    assert!(fields.contains(&"five"));
    assert!(fields.contains(&"fifteen"));
}

#[test]
fn load_provider_executes() {
    let p = LoadProvider;
    let (_, result) = p
        .execute(None)
        .into_iter()
        .next()
        .expect("Load should always succeed");
    let one = result.get("one").unwrap().as_text();
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
        let _ = p.execute(None); // Should not panic even on VMs with no battery
    }
}

// --- Uptime ---

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod uptime_tests {
    use beachcomber::provider::FieldScope;
    use beachcomber::provider::Provider;
    use beachcomber::provider::uptime::UptimeProvider;

    #[test]
    fn uptime_provider_metadata() {
        let p = UptimeProvider;
        let meta = p.metadata();
        assert_eq!(meta.name, "uptime");
        assert_eq!(meta.inferred_scope(), FieldScope::Global);
        let fields: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(fields.contains(&"seconds"));
        assert!(fields.contains(&"days"));
        assert!(fields.contains(&"hours"));
        assert!(fields.contains(&"minutes"));
    }

    #[test]
    fn uptime_provider_executes() {
        let p = UptimeProvider;
        // sysctl may be unavailable in sandboxed environments; accept None.
        // If Some, validate the primary field is present.
        if let Some((_, result)) = p.execute(None).into_iter().next() {
            assert!(
                result.get("seconds").is_some(),
                "seconds field should be present"
            );
        }
    }
}
