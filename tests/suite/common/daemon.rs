use crate::common::socket::IsolatedSocket;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// Run `cmd`, a `comb daemon` command line, and return once the daemon says
/// it serves ([`libbeachcomber::spawn::launch`], the readiness pipe every
/// spawn waits on). Its stderr is the test's own. The `Child` is the
/// caller's to [`stop`].
#[allow(dead_code)]
pub fn launch(cmd: &mut Command) -> Child {
    libbeachcomber::spawn::launch(cmd, None, libbeachcomber::spawn::READY_TIMEOUT)
        .unwrap_or_else(|e| panic!("comb daemon did not start: {e}"))
}

/// Stop a spawned daemon as `comb kill` does, with SIGTERM: it shuts down
/// cleanly, so an instrumented build writes its coverage profile, which a
/// SIGKILL never lets it. One that has not exited within a second is
/// SIGKILLed. Either way it is reaped.
#[allow(dead_code)]
pub fn stop(process: &mut Child) {
    // SAFETY: a signal to a child not yet reaped, so its pid is still its own.
    unsafe { libc::kill(process.id() as libc::pid_t, libc::SIGTERM) };
    let exited = crate::common::poll::poll_for(
        std::time::Duration::from_secs(1),
        std::time::Duration::from_millis(1),
        || !matches!(process.try_wait(), Ok(None)),
    );
    if !exited {
        let _ = process.kill();
    }
    let _ = process.wait();
}

/// Point `cmd` at a throwaway `$HOME`, `$XDG_CONFIG_HOME`, `$XDG_STATE_HOME`
/// (= `<home>/.local/state`) and `$BEACHCOMBER_SOCKET` (= `sock`), with
/// `RUST_LOG=error`, and `$KUBECONFIG` and `$TALOSCONFIG` removed, so nothing it runs can read or write the developer's real
/// config, defaults, log or daemon; and put [`stub_bin`] first on its `PATH`,
/// so no template it renders runs the developer's own tools.
pub fn isolate<'a>(cmd: &'a mut Command, home: &Path, xdg: &Path, sock: &Path) -> &'a mut Command {
    let path = std::env::join_paths(std::iter::once(stub_bin().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("a PATH of directories");
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", xdg)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("BEACHCOMBER_SOCKET", sock)
        .env("RUST_LOG", "error")
        .env("PATH", path)
        // The variables the shipped templates read (`kubecontext`, `talos`):
        // the developer's own name their real config files.
        .env_remove("KUBECONFIG")
        .env_remove("TALOSCONFIG")
}

/// The tools the shipped templates run through `exec()` — `kubectl` and
/// `talosctl` — as stand-ins that exit 1, as a tool with nothing to report
/// does: what [`isolate`] puts first on `PATH`. The developer's own would
/// make every `comb status` call a cluster, and its answer depend on the
/// machine. A test about those tools puts a `bin/` of its own first.
///
/// Each counts itself: with `$COMB_STUB_LOG` set in the environment it runs
/// in, an invocation appends one line, the tool's name and its arguments, to
/// that file before it exits 1, so a test can count the processes a command
/// spawned. Unset, it only exits 1.
pub fn stub_bin() -> &'static Path {
    use std::os::unix::fs::PermissionsExt;
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("stub-bin");
        std::fs::create_dir_all(&dir).expect("make the stub bin");
        for tool in ["kubectl", "talosctl"] {
            // Written beside and renamed over, so a test process that looks
            // while another writes sees the whole script or none.
            let part = dir.join(format!(".{tool}.{}", std::process::id()));
            std::fs::write(&part, stub_script(tool)).expect("write a stub");
            std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755))
                .expect("make a stub executable");
            std::fs::rename(&part, dir.join(tool)).expect("put a stub in place");
        }
        dir
    })
}

/// The text of the stub `tool`: one line to `$COMB_STUB_LOG` when it is set,
/// then exit 1.
fn stub_script(tool: &str) -> String {
    format!(
        "#!/bin/sh\n\
         if [ -n \"$COMB_STUB_LOG\" ]; then echo \"{tool} $*\" >> \"$COMB_STUB_LOG\"; fi\n\
         exit 1\n"
    )
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

/// An in-process daemon: the runtime it runs on, a single-threaded one on a
/// thread of its own, so that a test can move its clock; and the token that
/// stops it.
struct InProcess {
    runtime: tokio::runtime::Handle,
    cancel: tokio_util::sync::CancellationToken,
    thread: std::thread::JoinHandle<()>,
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
        // `--exit-with-parent`: the daemon self-terminates if this test process
        // dies without running `Drop` (e.g. nextest SIGKILLs a timed-out test),
        // so a daemon can never be orphaned. Drop below still handles the normal
        // and panic-unwind paths.
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_comb"));
        isolate(&mut cmd, &home, &xdg, &socket.path)
            .args(["daemon", "--exit-with-parent", "--socket"])
            .arg(&socket.path);
        let process = launch(&mut cmd);
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
    /// daemon's is, the managed defaults are written as `comb daemon` does
    /// before it loads its configuration, and its env file is loaded before
    /// the runtime starts, as `comb daemon` loads it. Nextest gives each test
    /// its own process, so setting it here is local to the test.
    ///
    /// It runs on a single-threaded runtime of its own, whose clock
    /// [`TestDaemon::advance`] moves.
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
        let config = beachcomber::config::Config::load();
        // SAFETY: as above; the runtime that starts threads is built next.
        unsafe { config.load_env_file() };

        let cancel = tokio_util::sync::CancellationToken::new();
        let (started_tx, started) = std::sync::mpsc::channel();
        let (path, stop) = (socket.path.clone(), cancel.clone());
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("daemon runtime");
            runtime.block_on(async move {
                let (task, ready) =
                    beachcomber::daemon::start_in_process_polling_with(path, config, stop, None);
                let ready = ready.await;
                let _ = started_tx.send((tokio::runtime::Handle::current(), ready));
                let _ = task.await;
            });
            // A Source's run still under way on a blocking thread is not
            // waited for.
            runtime.shutdown_background();
        });
        let (runtime, ready) = started.recv().expect("the daemon's thread started it");
        ready.expect("the in-process daemon refused to start");
        Self {
            socket,
            home,
            xdg,
            process: None,
            served: Some(InProcess {
                runtime,
                cancel,
                thread,
            }),
        }
    }

    /// Move this in-process daemon's clock on by `by` at once: the clock its
    /// scheduler ticks on and its lifecycle, its failure backoff and its
    /// records' ages read. Whatever falls due in that time is done on the
    /// daemon's next turn, as if `by` had passed; real time goes on from
    /// there.
    #[allow(dead_code)]
    pub fn advance(&self, by: std::time::Duration) {
        let served = self
            .served
            .as_ref()
            .expect("advance() moves an in-process daemon's clock");
        let (done, advanced) = std::sync::mpsc::channel();
        served.runtime.spawn(async move {
            tokio::time::pause();
            tokio::time::advance(by).await;
            tokio::time::resume();
            let _ = done.send(());
        });
        advanced.recv().expect("the daemon's clock advanced");
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
            stop(process);
        }
        if let Some(served) = self.served.take() {
            served.cancel.cancel();
            let _ = served.thread.join();
        }
    }
}
