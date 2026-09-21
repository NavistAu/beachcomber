// `parse_pmset_output` is the macOS pmset parser (gated `#[cfg(target_os = "macos")]`
// in the provider). These tests exercise it directly, so the whole file is macOS-only;
// on Linux it compiles to nothing (Linux battery parsing is covered elsewhere).
#![cfg(target_os = "macos")]

use beachcomber::provider::Value;
use beachcomber::provider::battery::parse_pmset_output;

#[test]
fn pmset_charging_with_estimate() {
    let sample = " -InternalBattery-0 (id=1234567)\t85%; charging; 1:23 remaining present: true\n";
    let r = parse_pmset_output(sample).expect("parses");
    assert_eq!(r.get("percent").unwrap(), &Value::Int(85));
    assert_eq!(r.get("charging").unwrap(), &Value::Bool(true));
    assert_eq!(r.get("time_remaining_secs").unwrap(), &Value::Int(4980)); // 1*3600 + 23*60
    assert_eq!(r.get("status").unwrap().as_text(), "charging");
}

#[test]
fn pmset_discharging_no_estimate() {
    let sample = " -InternalBattery-0\t72%; discharging; (no estimate) present: true\n";
    let r = parse_pmset_output(sample).expect("parses");
    assert_eq!(r.get("percent").unwrap(), &Value::Int(72));
    assert_eq!(r.get("time_remaining_secs").unwrap(), &Value::Int(0));
    assert_eq!(r.get("status").unwrap().as_text(), "calculating");
}

#[test]
fn pmset_charged_state() {
    let sample = " -InternalBattery-0\t100%; charged; 0:00 remaining present: true\n";
    let r = parse_pmset_output(sample).expect("parses");
    assert_eq!(r.get("status").unwrap().as_text(), "charged");
}
