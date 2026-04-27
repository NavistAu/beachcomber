use beachcomber::boundaries::process::{ProcessExecutor, RealProcessExecutor};

#[test]
fn real_executor_runs_echo() {
    let exec = RealProcessExecutor;
    let out = exec.run("echo", &["hi"]).unwrap();
    assert_eq!(out.stdout, b"hi\n");
    assert!(out.status.success());
}

#[test]
fn real_executor_with_input_pipes_stdin() {
    let exec = RealProcessExecutor;
    let out = exec.run_with_input("cat", &[], b"hello").unwrap();
    assert_eq!(out.stdout, b"hello");
    assert!(out.status.success());
}
