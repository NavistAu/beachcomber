use crate::common::poll::poll_until;
use crate::common::socket::IsolatedSocket;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

#[allow(dead_code)]
pub struct TestDaemon {
    pub socket: IsolatedSocket,
    /// The daemon's `$HOME`, a directory inside the socket's tempdir.
    pub home: PathBuf,
    /// The daemon's `$XDG_CONFIG_HOME`; its config dir is `xdg/beachcomber`.
    pub xdg: PathBuf,
    process: Child,
}

impl TestDaemon {
    // Used by cli_golden.rs integration tests.
    #[allow(dead_code)]
    pub fn spawn() -> Self {
        let socket = IsolatedSocket::new();
        // The daemon writes its managed defaults into `$XDG_CONFIG_HOME` on
        // start and its log under `$XDG_STATE_HOME`, so those and `$HOME` all
        // point into the socket's tempdir rather than at the developer's real
        // `~/.config/beachcomber` and `~/.local/state/beachcomber`.
        let home = socket.dir.path().join("home");
        let xdg = socket.dir.path().join("xdg");
        Self::spawn_in(socket, home, xdg)
    }

    /// Spawn with a chosen `$HOME` and `$XDG_CONFIG_HOME`, for a test that
    /// writes into the daemon's config dir (`xdg/beachcomber`) before it starts.
    /// `$XDG_STATE_HOME` is `<home>/.local/state` — the shell's own value would
    /// otherwise reach the daemon and its log would land in the real one.
    #[allow(dead_code)]
    pub fn spawn_in(socket: IsolatedSocket, home: PathBuf, xdg: PathBuf) -> Self {
        std::fs::create_dir_all(&home).expect("create temp HOME");
        // The Child is stored in the returned struct and wait()ed in the Drop impl.
        #[allow(clippy::zombie_processes)]
        // `--exit-with-parent`: the daemon self-terminates if this test process
        // dies without running `Drop` (e.g. nextest SIGKILLs a timed-out test),
        // so a daemon can never be orphaned. Drop below still handles the normal
        // and panic-unwind paths.
        let process = Command::new(env!("CARGO_BIN_EXE_comb"))
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &xdg)
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .args(["daemon", "--exit-with-parent", "--socket"])
            .arg(&socket.path)
            .spawn()
            .expect("spawn comb daemon");

        poll_until(
            &format!("daemon socket at {:?}", socket.path),
            "the daemon to bind it",
            Duration::from_secs(5),
            Duration::from_millis(50),
            || socket.path.exists(),
            |exists| *exists,
        );
        Self {
            socket,
            home,
            xdg,
            process,
        }
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}
