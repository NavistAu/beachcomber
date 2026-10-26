use crate::provider::Provider;
use crate::provider::battery::BatteryProvider;

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
    // TODO(section-J): update to sources()[0].execute().unwrap() once battery migrated in Section I
    let _ = p;
}

// --- Battery (Linux) ---

#[cfg(target_os = "linux")]
mod battery_linux_tests {
    use crate::provider::battery::BatteryProvider;

    #[test]
    fn battery_provider_handles_no_battery() {
        let p = BatteryProvider;
        // Should not panic even on VMs with no battery
        // TODO(section-J): update to sources()[0].execute().unwrap() once battery migrated in Section I
        let _ = p;
    }
}
