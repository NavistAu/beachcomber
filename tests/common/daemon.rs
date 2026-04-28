use crate::common::socket::IsolatedSocket;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

#[allow(dead_code)]
pub struct TestDaemon {
    pub socket: IsolatedSocket,
    process: Child,
}

impl TestDaemon {
    // Used by cli_golden.rs integration tests.
    #[allow(dead_code)]
    pub fn spawn() -> Self {
        let socket = IsolatedSocket::new();
        // The Child is stored in the returned struct and wait()ed in the Drop impl.
        #[allow(clippy::zombie_processes)]
        let process = Command::new(env!("CARGO_BIN_EXE_comb"))
            .args(["daemon", "--socket"])
            .arg(&socket.path)
            .spawn()
            .expect("spawn comb daemon");

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if socket.path.exists() {
                return Self { socket, process };
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("daemon never created socket at {:?}", socket.path);
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}
