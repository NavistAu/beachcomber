use std::process::Output;

#[cfg_attr(test, mockall::automock)]
pub trait ProcessExecutor: Send + Sync {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<Output>;
    fn run_with_input(&self, program: &str, args: &[&str], stdin: &[u8])
    -> std::io::Result<Output>;
}

pub struct RealProcessExecutor;

impl ProcessExecutor for RealProcessExecutor {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<Output> {
        std::process::Command::new(program).args(args).output()
    }

    fn run_with_input(
        &self,
        program: &str,
        args: &[&str],
        stdin: &[u8],
    ) -> std::io::Result<Output> {
        use std::io::Write;
        let mut child = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        if let Some(mut s) = child.stdin.take() {
            s.write_all(stdin)?;
        }
        child.wait_with_output()
    }
}
