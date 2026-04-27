use beachcomber::boundaries::process::{ProcessExecutor, RealProcessExecutor};

#[test]
fn real_executor_runs_echo() {
    let exec = RealProcessExecutor;
    let out = exec.run("echo", vec!["hi".to_string()]).unwrap();
    assert_eq!(out.stdout, b"hi\n");
    assert!(out.status.success());
}

#[test]
fn real_executor_with_input_pipes_stdin() {
    let exec = RealProcessExecutor;
    let out = exec
        .run_with_input("cat", vec![], b"hello".to_vec())
        .unwrap();
    assert_eq!(out.stdout, b"hello");
    assert!(out.status.success());
}
