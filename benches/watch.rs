//! The watch fan-in (phase 3 of the one-tree rebuild, targets T3.1–T3.5).
//!
//! The daemon is the release `comb daemon`, spawned as its own process under
//! a throwaway `$HOME` and `$XDG_*`, so what this process measures of itself
//! — its sleeps and its CPU while a watch idles — is the client's alone. The
//! watches are `libbeachcomber::watch::ResolvedWatch`, the fan-in behind
//! `comb watch` and `bc_watch_*`, on the global entry; the writes that drive
//! them are `put`s on a raw socket connection held open.
//!
//! A watch of `k` deps reads `f<k>.v`, a template naming `k - 1` refs (`k = 1`
//! is a plain stored key), and each change is a `put` to the last of them.
//!
//! Printed, each as min, p50, p90, p99 and max:
//!   * `delivery` (T3.1): from a `put` going out to `next` returning its
//!     value, at k = 1, 5 and 20. Target: p50 ≤ 1 ms, p99 ≤ 5 ms at every k.
//!   * `open` (T3.5): opening a warm 20-dep watch. Target: ≤ 5 ms.
//!   * `cancel` (T3.3): from `CancelHandle::cancel` to a `next(None)` blocked
//!     on another thread returning. Target: ≤ 2 ms.
//!   * `deadline` (T3.4): how far past `d` an idle 20-dep watch's `next(Some(d))`
//!     returns, for d = 10 ms and 50 ms; never before. Target: [d, d + 2 ms].
//!
//! And once each:
//!   * `idle` (T3.2): an open 20-dep watch's `next(Some(10 s))` with nothing
//!     happening: this process's sleeps (Linux `ru_nvcsw`; on macOS, which
//!     counts none, its BSD system calls) and CPU. Target: ≤ 5 and ≤ 1 ms.
//!   * `comb_watch_idle` (T3.2): a `comb watch` of a 20-dep key left running
//!     for 60 s after its first frame, against one stopped at its first frame:
//!     its CPU and voluntary context switches from `wait4`, as `/usr/bin/time`
//!     reads them. Target: ≤ 10 ms of CPU for the minute.
//!
//! Run with `cargo bench --bench watch`. A filter runs one part: `-- delivery`,
//! `-- open`, `-- cancel`, `-- deadline`, `-- idle` or `-- comb_watch_idle`.

use libbeachcomber::resolve::{Inputs, Slot};
use libbeachcomber::watch::{ResolvedWatch, WatchOutcome};
use libbeachcomber::{Client, ClientConfig};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// The dep counts T3.1 is written for.
const DEPS: [usize; 3] = [1, 5, 20];

/// Samples per printed measurement.
const ROUNDS: usize = 500;

/// A release `comb daemon` in a directory of its own, stopped when dropped.
struct Daemon {
    process: Child,
    dir: TempDir,
    sock: PathBuf,
}

impl Daemon {
    fn start() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let sock = dir.path().join("s");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_comb"));
        isolate(&mut cmd, dir.path(), &sock)
            .args(["daemon", "--socket"])
            .arg(&sock)
            .stderr(Stdio::null());
        let process =
            libbeachcomber::spawn::launch(&mut cmd, None, libbeachcomber::spawn::READY_TIMEOUT)
                .unwrap_or_else(|e| panic!("comb daemon did not start: {e:?}"));
        Self { process, dir, sock }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    /// A client of this daemon. Its reads wait up to 5 s, not the default
    /// 100 ms: on a loaded machine one slow read would end a part, and the
    /// percentiles are what show the slow ones.
    fn client(&self) -> Client {
        Client::with_config(ClientConfig {
            timeout: Duration::from_secs(5),
            auto_start: false,
            exec: false,
        })
        .with_socket_path(self.sock.clone())
    }

    fn writer(&self) -> Writer {
        let stream = UnixStream::connect(&self.sock).expect("connect");
        Writer {
            reader: BufReader::new(stream.try_clone().expect("clone")),
            stream,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // SAFETY: a signal to a child not yet reaped, so its pid is its own.
        unsafe { libc::kill(self.process.id() as libc::pid_t, libc::SIGTERM) };
        let _ = self.process.wait();
    }
}

/// `cmd` under `dir`'s own `$HOME` and `$XDG_*`, on `sock`.
fn isolate<'a>(cmd: &'a mut Command, dir: &Path, sock: &Path) -> &'a mut Command {
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("home");
    cmd.env("HOME", &home)
        .env("XDG_CONFIG_HOME", dir.join("xdg"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("BEACHCOMBER_SOCKET", sock)
        .env("RUST_LOG", "error")
}

/// `put`s on one connection.
struct Writer {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Writer {
    fn put(&mut self, key: &str, data: Value) {
        let mut line = json!({"op": "put", "key": key, "data": data}).to_string();
        line.push('\n');
        self.stream.write_all(line.as_bytes()).expect("send");
        let mut answer = String::new();
        self.reader.read_line(&mut answer).expect("answer");
        assert!(answer.contains("\"ok\":true"), "put {key}: {answer}");
    }
}

/// The key a watch of `k` deps reads, and the dep each change is put to,
/// seeded.
fn seed(writer: &mut Writer, k: usize) -> (String, String) {
    if k == 1 {
        writer.put("w1", json!({"x": "0"}));
        return ("w1.x".to_string(), "w1.x".to_string());
    }
    let refs: Vec<String> = (0..k - 1).map(|i| format!("r{k}_{i}")).collect();
    for r in &refs {
        writer.put(r, json!({"x": "a"}));
    }
    let template = refs
        .iter()
        .map(|r| format!("comb.{r}.x"))
        .collect::<Vec<_>>()
        .join(" ~ ");
    writer.put(
        &format!("f{k}:template"),
        json!({"v": format!("{{{{ {template} }}}}")}),
    );
    (
        format!("f{k}.v"),
        format!("{}.x", refs.last().expect("a ref")),
    )
}

/// The resolution inputs of every watch here: the global entry.
fn inputs<'a>(home: &'a str, env: &'a HashMap<String, String>) -> Inputs<'a> {
    Inputs {
        cwd: home,
        env,
        slot: Slot::Global,
        force: false,
        wait: libbeachcomber::Wait::Budget,
        allow_exec: false,
        now: std::time::SystemTime::now(),
    }
}

/// A watch on `key` with its first value taken and its subscriptions'
/// opening frames handled, so the next frame is a change.
fn settled<'a>(
    client: &'a Client,
    key: &str,
    inputs: &Inputs<'_>,
    deps: usize,
) -> ResolvedWatch<'a> {
    let mut w = ResolvedWatch::open(client, key, inputs).expect("open");
    assert_eq!(w.deps().len(), deps, "{key}: {:?}", w.deps());
    assert!(matches!(
        w.next(Some(Duration::ZERO)),
        Ok(WatchOutcome::Value(Some(_)))
    ));
    while !matches!(
        w.next(Some(Duration::from_millis(100))),
        Ok(WatchOutcome::Timeout)
    ) {}
    w
}

/// `samples` as min, p50, p90, p99 and max.
fn report(label: &str, mut samples: Vec<Duration>) {
    samples.sort();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!(
        "  {label:<14} min {:>7.3}ms  p50 {:>7.3}ms  p90 {:>7.3}ms  p99 {:>7.3}ms  max {:>7.3}ms  (n={})",
        ms(samples[0]),
        ms(at(0.5)),
        ms(at(0.9)),
        ms(at(0.99)),
        ms(samples[samples.len() - 1]),
        samples.len()
    );
}

/// T3.1: from a `put` going out to `next` returning it.
fn delivery(daemon: &Daemon, writer: &mut Writer) {
    println!("delivery: put to next returning the new value ({ROUNDS} rounds per k)");
    let client = daemon.client();
    let home = daemon.home().to_string_lossy().into_owned();
    let env = HashMap::new();
    for k in DEPS {
        let (key, dep) = seed(writer, k);
        let mut w = settled(&client, &key, &inputs(&home, &env), k);
        let mut samples = Vec::with_capacity(ROUNDS);
        for round in 0..ROUNDS {
            let value = format!("v{round}");
            let t0 = Instant::now();
            writer.put(&dep, json!(value));
            match w.next(Some(Duration::from_secs(5))) {
                Ok(WatchOutcome::Value(Some(_))) => samples.push(t0.elapsed()),
                other => panic!("k={k}: expected the new value, got {other:?}"),
            }
        }
        report(&format!("k={k}"), samples);
    }
    let mut puts = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        let t0 = Instant::now();
        writer.put("alone", json!({"x": round}));
        puts.push(t0.elapsed());
    }
    report("put alone", puts);
}

/// T3.5: opening a warm 20-dep watch.
fn open(daemon: &Daemon, writer: &mut Writer) {
    println!("open: ResolvedWatch::open of a warm 20-dep key ({ROUNDS} rounds)");
    let client = daemon.client();
    let home = daemon.home().to_string_lossy().into_owned();
    let env = HashMap::new();
    let (key, _) = seed(writer, 20);
    drop(ResolvedWatch::open(&client, &key, &inputs(&home, &env)).expect("open"));
    let mut samples = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let t0 = Instant::now();
        let w = ResolvedWatch::open(&client, &key, &inputs(&home, &env)).expect("open");
        samples.push(t0.elapsed());
        drop(w);
    }
    report("k=20", samples);
}

/// T3.3: a cancel releasing a `next(None)` blocked on another thread.
fn cancel(daemon: &Daemon, writer: &mut Writer) {
    const CANCELS: usize = 200;
    println!("cancel: cancel() to a blocked next(None) returning ({CANCELS} rounds, 20 deps)");
    let client = daemon.client();
    let home = daemon.home().to_string_lossy().into_owned();
    let env = HashMap::new();
    let (key, _) = seed(writer, 20);
    let mut samples = Vec::with_capacity(CANCELS);
    for _ in 0..CANCELS {
        let mut w = settled(&client, &key, &inputs(&home, &env), 20);
        let handle = w.cancel_handle();
        let (returned_at, cancelled_at) = std::thread::scope(|scope| {
            let blocked = scope.spawn(|| {
                let outcome = w.next(None);
                (Instant::now(), outcome)
            });
            // Long enough for the other thread to be in its wait.
            std::thread::sleep(Duration::from_millis(5));
            let at = Instant::now();
            handle.cancel();
            let (returned_at, outcome) = blocked.join().expect("the blocked thread");
            assert!(
                matches!(outcome, Ok(WatchOutcome::Cancelled)),
                "{outcome:?}"
            );
            (returned_at, at)
        });
        samples.push(returned_at.saturating_duration_since(cancelled_at));
    }
    report("k=20", samples);
}

/// T3.4: how far past its deadline an idle `next(Some(d))` returns.
fn deadline(daemon: &Daemon, writer: &mut Writer) {
    const WAITS: usize = 100;
    println!("deadline: next(Some(d)) on an idle 20-dep watch, time past d ({WAITS} rounds per d)");
    let client = daemon.client();
    let home = daemon.home().to_string_lossy().into_owned();
    let env = HashMap::new();
    let (key, _) = seed(writer, 20);
    let mut w = settled(&client, &key, &inputs(&home, &env), 20);
    for d in [Duration::from_millis(10), Duration::from_millis(50)] {
        let mut over = Vec::with_capacity(WAITS);
        for _ in 0..WAITS {
            let t0 = Instant::now();
            let outcome = w.next(Some(d));
            let took = t0.elapsed();
            assert!(matches!(outcome, Ok(WatchOutcome::Timeout)), "{outcome:?}");
            assert!(took >= d, "returned {took:?} into a {d:?} wait");
            over.push(took - d);
        }
        report(&format!("d={}ms", d.as_millis()), over);
    }
}

/// T3.2: a 20-dep watch idle for ten seconds.
fn idle(daemon: &Daemon, writer: &mut Writer) {
    println!("idle: next(Some(10s)) on an open 20-dep watch with nothing happening");
    let client = daemon.client();
    let home = daemon.home().to_string_lossy().into_owned();
    let env = HashMap::new();
    let (key, _) = seed(writer, 20);
    let mut w = settled(&client, &key, &inputs(&home, &env), 20);
    let (sleeps, cpu) = (sleeps(), cpu());
    let outcome = w.next(Some(Duration::from_secs(10)));
    let (slept, used) = (self::sleeps() - sleeps, self::cpu() - cpu);
    assert!(matches!(outcome, Ok(WatchOutcome::Timeout)), "{outcome:?}");
    println!(
        "  20 streams, 10s   {slept} {}   {:.3}ms CPU",
        SLEEPS_ARE,
        used.as_secs_f64() * 1000.0
    );
}

/// T3.2: a `comb watch` idle for a minute, against one stopped at once.
fn comb_watch_idle(daemon: &Daemon, writer: &mut Writer) {
    println!(
        "comb_watch_idle: a `comb watch` of a 20-dep key, from wait4 as /usr/bin/time reads it"
    );
    let (key, _) = seed(writer, 20);
    for (label, idle) in [
        ("stopped at once", Duration::ZERO),
        ("idle 60s", Duration::from_secs(60)),
    ] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_comb"));
        isolate(&mut cmd, daemon.dir.path(), &daemon.sock)
            .args(["watch", "--global", &key])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Reaped by `reap`, with `wait4`, which `Child::wait` cannot do.
        #[allow(clippy::zombie_processes)]
        let mut child = cmd.spawn().expect("spawn comb watch");
        let mut first = String::new();
        BufReader::new(child.stdout.take().expect("stdout"))
            .read_line(&mut first)
            .expect("the first frame");
        std::thread::sleep(idle);
        // SAFETY: a signal to a child not yet reaped, so its pid is its own.
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        let (cpu, nvcsw) = reap(&child);
        println!(
            "  {label:<16} {:>8.3}ms CPU   {nvcsw} voluntary context switches",
            cpu.as_secs_f64() * 1000.0
        );
    }
}

/// Reap `child` with `wait4`, as `/usr/bin/time` does: its CPU (user plus
/// system) and its voluntary context switches.
fn reap(child: &Child) -> (Duration, libc::c_long) {
    let mut status = 0;
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `child` is ours and not yet reaped; `usage` is writable.
    let pid = unsafe {
        libc::wait4(
            child.id() as libc::pid_t,
            &mut status,
            0,
            usage.as_mut_ptr(),
        )
    };
    assert_eq!(pid, child.id() as libc::pid_t, "wait4");
    // SAFETY: `wait4` succeeded, so it filled `usage`.
    let usage = unsafe { usage.assume_init() };
    (
        timeval(usage.ru_utime) + timeval(usage.ru_stime),
        usage.ru_nvcsw,
    )
}

fn timeval(t: libc::timeval) -> Duration {
    Duration::from_secs(t.tv_sec as u64) + Duration::from_micros(t.tv_usec as u64)
}

/// This process's CPU so far, user plus system.
fn cpu() -> Duration {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `usage` is writable.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    // SAFETY: `getrusage` succeeded, so it filled `usage`.
    let usage = unsafe { usage.assume_init() };
    timeval(usage.ru_utime) + timeval(usage.ru_stime)
}

/// What [`sleeps`] counts, for the report.
#[cfg(target_os = "linux")]
const SLEEPS_ARE: &str = "voluntary context switches";
#[cfg(target_os = "macos")]
const SLEEPS_ARE: &str = "BSD system calls (macOS counts no voluntary switches)";

/// A count that grows by one each time a thread of this process goes back to
/// sleep in the kernel: Linux's voluntary context switches; on macOS, which
/// keeps no such count, its BSD system calls, since a thread that wakes from
/// a wait makes another to sleep again.
#[cfg(target_os = "linux")]
fn sleeps() -> i64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `usage` is writable.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    // SAFETY: `getrusage` succeeded, so it filled `usage`.
    i64::from(unsafe { usage.assume_init() }.ru_nvcsw)
}

#[cfg(target_os = "macos")]
fn sleeps() -> i64 {
    /// `task_events_info`, `mach/task_info.h`; the kernel fills every field.
    #[repr(C)]
    #[derive(Default)]
    #[allow(dead_code)]
    struct TaskEventsInfo {
        faults: libc::integer_t,
        pageins: libc::integer_t,
        cow_faults: libc::integer_t,
        messages_sent: libc::integer_t,
        messages_received: libc::integer_t,
        syscalls_mach: libc::integer_t,
        syscalls_unix: libc::integer_t,
        csw: libc::integer_t,
    }
    const TASK_EVENTS_INFO: libc::task_flavor_t = 2;
    let mut info = TaskEventsInfo::default();
    let mut count = (std::mem::size_of::<TaskEventsInfo>() / std::mem::size_of::<libc::natural_t>())
        as libc::mach_msg_type_number_t;
    // SAFETY: `info` is a writable `task_events_info` of `count` words.
    // `mach_task_self` is deprecated in libc for the `mach2` crate, which
    // this one measurement does not warrant.
    #[allow(deprecated)]
    let kr = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            TASK_EVENTS_INFO,
            (&raw mut info).cast(),
            &mut count,
        )
    };
    assert_eq!(kr, libc::KERN_SUCCESS);
    i64::from(info.syscalls_unix)
}

/// One printed measurement.
type Part = fn(&Daemon, &mut Writer);

fn main() {
    // A filter is the first argument that is not a flag (`cargo bench` passes
    // `--bench`); with none, every part runs.
    let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    let runs = |name: &str| filter.as_deref().is_none_or(|f| name == f);
    let daemon = Daemon::start();
    let mut writer = daemon.writer();
    let parts: [(&str, Part); 6] = [
        ("delivery", delivery),
        ("open", open),
        ("cancel", cancel),
        ("deadline", deadline),
        ("idle", idle),
        ("comb_watch_idle", comb_watch_idle),
    ];
    for (name, part) in parts {
        if runs(name) {
            part(&daemon, &mut writer);
        }
    }
}
