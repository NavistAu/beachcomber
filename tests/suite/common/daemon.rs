use crate::common::poll::poll_until;
use crate::common::socket::IsolatedSocket;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

/// Point `cmd` at a throwaway `$HOME`, `$XDG_CONFIG_HOME`, `$XDG_STATE_HOME`
/// (= `<home>/.local/state`) and `$BEACHCOMBER_SOCKET` (= `sock`), with
/// `RUST_LOG=error`, so nothing it runs can read or write the developer's real
/// config, defaults, log or daemon.
pub fn isolate<'a>(cmd: &'a mut Command, home: &Path, xdg: &Path, sock: &Path) -> &'a mut Command {
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", xdg)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("BEACHCOMBER_SOCKET", sock)
        .env("RUST_LOG", "error")
}

#[allow(dead_code)]
pub struct TestDaemon {
    pub socket: IsolatedSocket,
    /// The daemon's `$HOME`, a directory inside the socket's tempdir.
    pub home: PathBuf,
    /// The daemon's `$XDG_CONFIG_HOME`; its config dir is `xdg/beachcomber`.
    pub xdg: PathBuf,
    process: Option<Child>,
    /// Set when the daemon runs inside this test process (see
    /// [`TestDaemon::serve_in_process`]).
    served: Option<InProcess>,
}

/// An in-process daemon: its own runtime, and the token that stops it.
struct InProcess {
    runtime: tokio::runtime::Runtime,
    cancel: tokio_util::sync::CancellationToken,
    task: tokio::task::JoinHandle<()>,
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
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_comb"));
        let process = isolate(&mut cmd, &home, &xdg, &socket.path)
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
            process: Some(process),
            served: None,
        }
    }

    /// The same daemon, run inside this test process: its provider file
    /// watching polls instead of registering kernel watches, and it starts in
    /// milliseconds rather than the fraction of a second a spawned `comb
    /// daemon` takes. For a test that is about the daemon's behaviour, not its
    /// process; one that needs the real process (its own environment, its
    /// arguments, signals) uses [`TestDaemon::spawn_in`].
    ///
    /// The process environment is pointed at `home` and `xdg` as the spawned
    /// daemon's is, and the managed defaults are written as `comb daemon` does
    /// before it loads its configuration. Nextest gives each test its own
    /// process, so setting it here is local to the test.
    #[allow(dead_code)]
    pub fn serve_in_process(socket: IsolatedSocket, home: PathBuf, xdg: PathBuf) -> Self {
        std::fs::create_dir_all(&home).expect("create temp HOME");
        // SAFETY: one test per process, and nothing else reads the environment
        // until the daemon below starts.
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("XDG_CONFIG_HOME", &xdg);
            std::env::set_var("XDG_STATE_HOME", home.join(".local/state"));
        }
        let config_file = xdg.join("beachcomber").join("config.toml");
        beachcomber::daemon::defaults::write_managed_defaults(
            &beachcomber::config::Config::defaults_dir_for(&config_file),
            env!("BEACHCOMBER_VERSION"),
        )
        .expect("write managed defaults");

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("daemon runtime");
        let cancel = tokio_util::sync::CancellationToken::new();
        let (task, ready) = {
            let _enter = runtime.enter();
            beachcomber::daemon::start_in_process_polling_with(
                socket.path.clone(),
                beachcomber::config::Config::load(),
                cancel.clone(),
                None,
            )
        };
        runtime
            .block_on(ready)
            .expect("the in-process daemon refused to start");
        Self {
            socket,
            home,
            xdg,
            process: None,
            served: Some(InProcess {
                runtime,
                cancel,
                task,
            }),
        }
    }

    /// `comb` at `cwd`, isolated to this daemon's home/xdg, with
    /// `BEACHCOMBER_SOCKET` = this daemon's socket and `RUST_LOG=error`.
    #[allow(dead_code)]
    pub fn command(&self, cwd: &Path) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_comb"));
        isolate(&mut cmd, &self.home, &self.xdg, &self.socket.path).current_dir(cwd);
        cmd
    }

    /// `command(cwd)` wrapped as `assert_cmd::Command::from_std`.
    #[allow(dead_code)]
    pub fn comb(&self, cwd: &Path) -> assert_cmd::Command {
        assert_cmd::Command::from_std(self.command(cwd))
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if let Some(process) = self.process.as_mut() {
            let _ = process.kill();
            let _ = process.wait();
        }
        if let Some(served) = self.served.take() {
            served.cancel.cancel();
            let _ = served.runtime.block_on(served.task);
            served.runtime.shutdown_background();
        }
    }
}
