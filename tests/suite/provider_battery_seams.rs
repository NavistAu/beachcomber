/// Seam tests for battery providers.
///
/// These tests use a hand-written `StubProcessExecutor` to provide canned pmset
/// (macOS) or upower (Linux) output, so they run without those binaries installed.
use beachcomber::boundaries::process::ProcessExecutor;
use std::process::{ExitStatus, Output};
use std::sync::Arc;

// ── Hand-written test double ──────────────────────────────────────────────────

/// A `ProcessExecutor` that returns a pre-programmed sequence of outputs.
/// Each call to `run` pops from the front of the queue. If the queue is empty
/// the stub returns an IO error.
struct StubProcessExecutor {
    outputs: std::sync::Mutex<std::collections::VecDeque<std::io::Result<Output>>>,
}

impl StubProcessExecutor {
    fn new(outputs: Vec<std::io::Result<Output>>) -> Arc<Self> {
        Arc::new(Self {
            outputs: std::sync::Mutex::new(outputs.into()),
        })
    }
}

fn make_output(exit_code: i32, stdout: &[u8]) -> std::io::Result<Output> {
    use std::os::unix::process::ExitStatusExt;
    Ok(Output {
        status: ExitStatus::from_raw(exit_code << 8),
        stdout: stdout.to_vec(),
        stderr: vec![],
    })
}

fn success_output(stdout: &[u8]) -> std::io::Result<Output> {
    make_output(0, stdout)
}

// Only the macOS test module exercises a non-zero git/pmset exit; gating this
// keeps clippy's dead_code lint quiet on Linux (where the macOS module is cfg'd out).
#[cfg(target_os = "macos")]
fn failed_output() -> std::io::Result<Output> {
    make_output(1, b"")
}

fn io_error() -> std::io::Result<Output> {
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "binary not found",
    ))
}

impl ProcessExecutor for StubProcessExecutor {
    fn run(&self, _program: &str, _args: Vec<String>) -> std::io::Result<Output> {
        self.outputs
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(io_error)
    }

    fn run_with_input(
        &self,
        program: &str,
        args: Vec<String>,
        _stdin: Vec<u8>,
    ) -> std::io::Result<Output> {
        self.run(program, args)
    }
}

// ── macOS pmset seam tests ────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use beachcomber::provider::Value;
    use beachcomber::provider::battery::battery_state_source_with_executor;

    fn execute_with(stdout: &[u8]) -> beachcomber::provider::SourceResult {
        let stub = StubProcessExecutor::new(vec![success_output(stdout)]);
        let source = battery_state_source_with_executor(stub);
        source.execute(None)
    }

    #[test]
    fn pmset_charging_with_estimate_produces_correct_fields() {
        let sample =
            b" -InternalBattery-0 (id=1234567)\t85%; charging; 1:23 remaining present: true\n";
        let result = execute_with(sample);
        assert_eq!(result.fields.get("percent").unwrap(), &Value::Int(85));
        assert_eq!(result.fields.get("charging").unwrap(), &Value::Bool(true));
        assert_eq!(
            result.fields.get("time_remaining_secs").unwrap(),
            &Value::Int(4980)
        ); // 1*3600 + 23*60
        assert_eq!(result.fields.get("status").unwrap().as_text(), "charging");
    }

    #[test]
    fn pmset_discharging_no_estimate_produces_calculating_status() {
        let sample = b" -InternalBattery-0\t72%; discharging; (no estimate) present: true\n";
        let result = execute_with(sample);
        assert_eq!(result.fields.get("percent").unwrap(), &Value::Int(72));
        assert_eq!(result.fields.get("charging").unwrap(), &Value::Bool(false));
        assert_eq!(
            result.fields.get("time_remaining_secs").unwrap(),
            &Value::Int(0)
        );
        assert_eq!(
            result.fields.get("status").unwrap().as_text(),
            "calculating"
        );
    }

    #[test]
    fn pmset_charged_state_produces_charged_status() {
        let sample = b" -InternalBattery-0\t100%; charged; 0:00 remaining present: true\n";
        let result = execute_with(sample);
        assert_eq!(result.fields.get("percent").unwrap(), &Value::Int(100));
        assert_eq!(result.fields.get("status").unwrap().as_text(), "charged");
    }

    #[test]
    fn pmset_binary_not_found_returns_empty_result() {
        let stub = StubProcessExecutor::new(vec![io_error()]);
        let source = battery_state_source_with_executor(stub);
        let result = source.execute(None);
        assert!(
            result.fields.is_empty(),
            "IO error should produce empty result, got: {:?}",
            result.fields
        );
    }

    #[test]
    fn pmset_nonzero_exit_returns_empty_result() {
        let stub = StubProcessExecutor::new(vec![failed_output()]);
        let source = battery_state_source_with_executor(stub);
        let result = source.execute(None);
        assert!(
            result.fields.is_empty(),
            "non-zero exit should produce empty result, got: {:?}",
            result.fields
        );
    }

    #[test]
    fn pmset_malformed_output_returns_empty_result() {
        let stub = StubProcessExecutor::new(vec![success_output(b"no battery info here\n")]);
        let source = battery_state_source_with_executor(stub);
        let result = source.execute(None);
        assert!(
            result.fields.is_empty(),
            "malformed output should produce empty result, got: {:?}",
            result.fields
        );
    }

    #[test]
    fn pmset_discharging_with_time_remaining() {
        let sample = b" -InternalBattery-0\t50%; discharging; 2:30 remaining present: true\n";
        let result = execute_with(sample);
        assert_eq!(result.fields.get("percent").unwrap(), &Value::Int(50));
        assert_eq!(result.fields.get("charging").unwrap(), &Value::Bool(false));
        assert_eq!(
            result.fields.get("time_remaining_secs").unwrap(),
            &Value::Int(9000)
        ); // 2*3600 + 30*60
        assert_eq!(
            result.fields.get("status").unwrap().as_text(),
            "discharging"
        );
    }
}

// ── Linux upower seam tests ───────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use beachcomber::provider::battery::battery_upower_source_with_executor;

    /// Runs the upower source with two canned outputs: first for `upower -e`,
    /// second for `upower -i <path>`.
    fn execute_with_two(first: &[u8], second: &[u8]) -> beachcomber::provider::SourceResult {
        let stub = StubProcessExecutor::new(vec![success_output(first), success_output(second)]);
        let source = battery_upower_source_with_executor(stub);
        source.execute(None)
    }

    #[test]
    fn upower_not_found_returns_empty() {
        let stub = StubProcessExecutor::new(vec![io_error()]);
        let source = battery_upower_source_with_executor(stub);
        let result = source.execute(None);
        // time_remaining_secs and status are still written from sysfs; here
        // with no sysfs battery dir, status becomes "unknown".
        assert!(result.fields.contains_key("status"));
        assert!(result.fields.contains_key("time_remaining_secs"));
        assert_eq!(result.fields.get("status").unwrap().as_text(), "unknown");
    }

    #[test]
    fn upower_returns_zero_time_when_no_battery_line() {
        let stub = StubProcessExecutor::new(vec![success_output(
            b"/org/freedesktop/UPower/devices/ac_adapter\n",
        )]);
        let source = battery_upower_source_with_executor(stub);
        let result = source.execute(None);
        // No "battery" line → time_remaining_secs = 0
        assert_eq!(
            result.fields.get("time_remaining_secs").unwrap().as_text(),
            "0"
        );
    }

    #[test]
    fn upower_parses_hours_correctly() {
        let list = b"/org/freedesktop/UPower/devices/battery_BAT0\n";
        let info = b"  time to empty: 1.5 hours\n";
        let result = execute_with_two(list, info);
        assert_eq!(
            result.fields.get("time_remaining_secs").unwrap().as_text(),
            "5400"
        );
    }

    #[test]
    fn upower_parses_minutes_correctly() {
        let list = b"/org/freedesktop/UPower/devices/battery_BAT0\n";
        let info = b"  time to empty: 45.0 minutes\n";
        let result = execute_with_two(list, info);
        assert_eq!(
            result.fields.get("time_remaining_secs").unwrap().as_text(),
            "2700"
        );
    }
}
