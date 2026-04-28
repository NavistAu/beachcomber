/// Seam tests for the `op` (1Password) provider.
///
/// Uses a hand-written `StubProcessExecutor` to simulate `op whoami` responses
/// without requiring the 1Password CLI to be installed.
use beachcomber::boundaries::process::ProcessExecutor;
use beachcomber::provider::Value;
use beachcomber::provider::op::op_source_with_executor;
use std::process::{ExitStatus, Output};
use std::sync::Arc;

// ── Hand-written test double ──────────────────────────────────────────────────

struct StubProcessExecutor {
    result: std::io::Result<Output>,
}

impl StubProcessExecutor {
    fn success(stdout: &[u8]) -> Arc<Self> {
        use std::os::unix::process::ExitStatusExt;
        Arc::new(Self {
            result: Ok(Output {
                status: ExitStatus::from_raw(0),
                stdout: stdout.to_vec(),
                stderr: vec![],
            }),
        })
    }

    fn failure() -> Arc<Self> {
        use std::os::unix::process::ExitStatusExt;
        Arc::new(Self {
            result: Ok(Output {
                status: ExitStatus::from_raw(1 << 8),
                stdout: vec![],
                stderr: b"[ERROR] not signed in".to_vec(),
            }),
        })
    }

    fn not_found() -> Arc<Self> {
        Arc::new(Self {
            result: Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "op: No such file or directory",
            )),
        })
    }
}

impl ProcessExecutor for StubProcessExecutor {
    fn run(&self, _program: &str, _args: Vec<String>) -> std::io::Result<Output> {
        match &self.result {
            Ok(o) => Ok(Output {
                status: o.status,
                stdout: o.stdout.clone(),
                stderr: o.stderr.clone(),
            }),
            Err(e) => Err(std::io::Error::new(e.kind(), e.to_string())),
        }
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn op_signed_in_returns_true_and_email() {
    let whoami_json =
        br#"{"account_uuid":"abc123","email":"user@example.com","url":"my.1password.com"}"#;
    let stub = StubProcessExecutor::success(whoami_json);
    let source = op_source_with_executor(stub);
    let result = source.execute(None);

    assert_eq!(
        result.fields.get("signed_in").unwrap(),
        &Value::Bool(true),
        "signed_in should be true when op whoami succeeds"
    );
    assert_eq!(
        result.fields.get("account").unwrap().as_text(),
        "user@example.com",
        "account should be the email from op whoami JSON"
    );
}

#[test]
fn op_signed_in_falls_back_to_url_when_no_email() {
    let whoami_json = br#"{"account_uuid":"abc123","url":"myteam.1password.com"}"#;
    let stub = StubProcessExecutor::success(whoami_json);
    let source = op_source_with_executor(stub);
    let result = source.execute(None);

    assert_eq!(result.fields.get("signed_in").unwrap(), &Value::Bool(true));
    assert_eq!(
        result.fields.get("account").unwrap().as_text(),
        "myteam.1password.com"
    );
}

#[test]
fn op_not_signed_in_returns_false_and_empty_account() {
    let stub = StubProcessExecutor::failure();
    let source = op_source_with_executor(stub);
    let result = source.execute(None);

    assert_eq!(
        result.fields.get("signed_in").unwrap(),
        &Value::Bool(false),
        "signed_in should be false when op whoami fails"
    );
    assert_eq!(
        result.fields.get("account").unwrap().as_text(),
        "",
        "account should be empty string when not signed in"
    );
}

#[test]
fn op_binary_not_installed_returns_false() {
    let stub = StubProcessExecutor::not_found();
    let source = op_source_with_executor(stub);
    let result = source.execute(None);

    assert_eq!(result.fields.get("signed_in").unwrap(), &Value::Bool(false));
    assert_eq!(result.fields.get("account").unwrap().as_text(), "");
}

#[test]
fn op_malformed_json_returns_signed_in_with_empty_account() {
    // Non-JSON stdout with a success exit code: provider should not panic.
    let stub = StubProcessExecutor::success(b"this is not json");
    let source = op_source_with_executor(stub);
    let result = source.execute(None);

    // signed_in is true (exit 0) but account parsing fails → empty string
    assert_eq!(result.fields.get("signed_in").unwrap(), &Value::Bool(true));
    assert_eq!(result.fields.get("account").unwrap().as_text(), "");
}

#[test]
fn op_empty_stdout_returns_signed_in_with_empty_account() {
    let stub = StubProcessExecutor::success(b"");
    let source = op_source_with_executor(stub);
    let result = source.execute(None);

    assert_eq!(result.fields.get("signed_in").unwrap(), &Value::Bool(true));
    assert_eq!(result.fields.get("account").unwrap().as_text(), "");
}
