//! The watch fan-in: one stream of the values `comb get` would print.
//!
//! A raw daemon watch answers about one address. What a caller asked to watch
//! is a *value*, and a value is what [`resolve`](crate::resolve) makes of
//! every key it reads — the addressed one, the refs its template names, and
//! the template tree itself, any of which can change under it. So this module
//! opens one daemon watch per key that resolution read, waits on all of them
//! at once, re-resolves on any frame and emits only when the resolved value
//! changed (design §5.5). The daemon notifies a namespace's watchers on every
//! write to it or to one of its facets, so a template edit wakes the key's own
//! watch.
//!
//! Three key classes, decided at [`ResolvedWatch::open`]:
//!
//! - `env` / `env.X` — constant for the process, so there is nothing to
//!   watch: [`WatchError::EnvKey`].
//! - a facet key, `p:facet.rest` — no template half, so no resolution: one
//!   plain daemon watch whose frames are emitted as they arrive.
//! - a provider key, `p.rest` — the fan-in.
//!
//! The library has no runtime, so the fan-in is synchronous: a wait is one
//! `poll(2)` over every stream plus the [`CancelHandle`]'s pipe
//! ([`wait_readable`]). A frame on any stream is seen the moment it lands, a
//! cancel from another thread ends the wait at once, and a watch with
//! nothing happening never wakes.

use crate::resolve::{Fetch, FetchError, Got, Inputs, ResolveError, Slot, resolve_traced};
use crate::{Client, CombError, Session, WatchStream};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::io::Write;
use std::ops::Deref;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// What [`wait_readable`] found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ready {
    /// These of the fds, by index, can be read without blocking. A hang-up or
    /// an error counts: the read that follows finds the end of the stream.
    Streams(Vec<usize>),
    /// The wake fd is readable: the wait was cancelled.
    Woken,
    /// The deadline passed with nothing to read.
    TimedOut,
}

/// Wait until one of `fds` can be read without blocking, `wake` is readable,
/// or `deadline` passes (`None`: no deadline). `wake` wins over the streams.
///
/// One `poll(2)`. A signal restarts it with the time that is left, and it
/// never answers [`Ready::TimedOut`] before the deadline: `poll` counts in
/// whole milliseconds, so the time left is rounded up, and a wake that came
/// early waits again.
pub(crate) fn wait_readable(
    fds: &[RawFd],
    wake: Option<RawFd>,
    deadline: Option<Instant>,
) -> std::io::Result<Ready> {
    let mut polled: Vec<libc::pollfd> = fds
        .iter()
        .chain(wake.as_ref())
        .map(|&fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    loop {
        let timeout = deadline.map_or(-1, |d| {
            poll_timeout(d.saturating_duration_since(Instant::now()))
        });
        // SAFETY: `polled` is an exclusively borrowed, initialised array of
        // `polled.len()` `pollfd`s that outlives the call.
        let n = unsafe { libc::poll(polled.as_mut_ptr(), polled.len() as libc::nfds_t, timeout) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if wake.is_some() && polled.last().is_some_and(|p| p.revents != 0) {
            return Ok(Ready::Woken);
        }
        let ready: Vec<usize> = polled[..fds.len()]
            .iter()
            .enumerate()
            .filter(|(_, p)| p.revents != 0)
            .map(|(i, _)| i)
            .collect();
        if !ready.is_empty() {
            return Ok(Ready::Streams(ready));
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(Ready::TimedOut);
        }
    }
}

/// `left` as `poll(2)`'s timeout: whole milliseconds, rounded up so a wait is
/// never shorter than asked. A wait longer than `poll` can express is made in
/// parts by [`wait_readable`]'s loop.
fn poll_timeout(left: Duration) -> libc::c_int {
    libc::c_int::try_from(left.as_nanos().div_ceil(1_000_000)).unwrap_or(libc::c_int::MAX)
}

/// Ends a [`ResolvedWatch::next`] wait, from any thread, at once.
///
/// A flag and a self-pipe: [`cancel`](Self::cancel) sets the flag and writes
/// one byte to the pipe, whose read end every wait polls beside its streams.
/// The byte is never read, so once cancelled, every later wait ends at once
/// too. Clones share the one flag and pipe.
#[derive(Clone, Debug)]
pub struct CancelHandle(Arc<Cancel>);

#[derive(Debug)]
struct Cancel {
    cancelled: AtomicBool,
    /// The pipe, as a socket pair: `wake` is polled, `signal` written. Both
    /// ends are non-blocking and close-on-exec.
    wake: UnixStream,
    signal: UnixStream,
}

impl CancelHandle {
    /// A handle nothing has cancelled. It fails only when the process is out
    /// of file descriptors.
    pub fn new() -> std::io::Result<Self> {
        let (wake, signal) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        signal.set_nonblocking(true)?;
        Ok(Self(Arc::new(Cancel {
            cancelled: AtomicBool::new(false),
            wake,
            signal,
        })))
    }

    /// End the wait in progress, if there is one, and every one after it.
    pub fn cancel(&self) {
        if !self.0.cancelled.swap(true, Ordering::SeqCst) {
            // The one byte a wait sees. It cannot block, and it cannot fail
            // while this handle holds both ends.
            let _ = (&self.0.signal).write(&[1]);
        }
    }

    /// Whether [`cancel`](Self::cancel) has been called.
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    /// The fd a wait polls to see the cancel.
    fn wake_fd(&self) -> RawFd {
        self.0.wake.as_raw_fd()
    }
}

/// What [`ResolvedWatch::next`] found.
#[derive(Clone, Debug, PartialEq)]
pub enum WatchOutcome {
    /// The value at the watched key, as `comb get` would print it. `None` is
    /// a miss: nothing is stored there, and the watch stays open.
    Value(Option<JsonValue>),
    /// The wait expired with nothing new.
    Timeout,
    /// The daemon closed the connection.
    Eof,
    /// The cancel handle was set.
    Cancelled,
}

/// Why a watch could not be opened, or could not go on.
#[derive(Debug)]
pub enum WatchError {
    /// `env` or `env.X`: the value is constant for the process (design §5.5).
    EnvKey(String),
    /// A resolution failed. A [`Config`](ResolveError::Config) error is a
    /// template someone has to go and edit, so the watch ends on it rather
    /// than emitting again.
    Resolve(ResolveError),
    /// The daemon could not be reached, or refused the watched key itself.
    Transport(CombError),
}

impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WatchError::EnvKey(m) => f.write_str(m),
            WatchError::Resolve(e) => e.fmt(f),
            WatchError::Transport(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for WatchError {}

impl WatchError {
    /// The failure's slug ([`crate::kind`]). A watch on `env` is the
    /// caller's input, `parse_error`; the rest classify as a resolution and
    /// a transport do.
    pub fn kind(&self) -> &'static str {
        match self {
            WatchError::EnvKey(_) => crate::kind::PARSE_ERROR,
            WatchError::Resolve(e) => e.kind(),
            WatchError::Transport(e) => e.kind(),
        }
    }
}

/// The [`Client`] a watch drives: borrowed, or owned by the watch itself.
///
/// A watch outlives the call that opened it, and a handle that crosses the C
/// ABI (`libbeachcomber-ffi`'s `BcWatch`) cannot carry a borrow — so
/// [`ResolvedWatch::open_owned`] takes an [`Arc`] and the watch keeps the
/// client alive for as long as it lives. Rust callers keep the cheaper borrow
/// through [`ResolvedWatch::open`].
enum ClientRef<'a> {
    Borrowed(&'a Client),
    Owned(Arc<Client>),
}

impl Deref for ClientRef<'_> {
    type Target = Client;

    fn deref(&self) -> &Client {
        match self {
            ClientRef::Borrowed(client) => client,
            ClientRef::Owned(client) => client,
        }
    }
}

/// Which of the three key classes a watch is.
enum Kind {
    /// A facet key: the daemon's frames, as they arrive.
    Raw,
    /// A provider key: the fan-in, re-resolved on every frame. A resolution
    /// reads each of its keys in turn, so it reads them over one connection
    /// of its own, held from one resolution to the next, rather than a
    /// connection per key. `None` after a resolution failed: its connection
    /// can still carry a late answer, so the next opens another.
    Resolved(Option<Session>),
}

/// [`Inputs`], owned — a watch outlives the call that opened it and
/// re-resolves with the same environment and directory every time.
struct OwnedInputs {
    cwd: String,
    env: HashMap<String, String>,
    slot: Slot,
    /// `force`/`wait` belong to the opening resolution — the one call
    /// [`OwnedInputs::as_inputs`] backs — and a subscription must not repeat
    /// them on its own re-resolves ([`OwnedInputs::as_reresolve_inputs`]).
    /// For a provider that changes on every execution (uptime, timestamps),
    /// replaying `force` into an internal re-resolve is a self-sustaining
    /// storm: frame → re-resolve(force) → execute → new value → frame.
    force: bool,
    wait: bool,
    allow_exec: bool,
}

impl OwnedInputs {
    fn new(inputs: &Inputs<'_>) -> Self {
        Self {
            cwd: inputs.cwd.to_string(),
            env: inputs.env.clone(),
            slot: inputs.slot.clone(),
            force: inputs.force,
            wait: inputs.wait,
            allow_exec: inputs.allow_exec,
        }
    }

    fn as_inputs(&self) -> Inputs<'_> {
        Inputs {
            cwd: &self.cwd,
            env: &self.env,
            slot: self.slot.clone(),
            force: self.force,
            wait: self.wait,
            allow_exec: self.allow_exec,
            now: std::time::SystemTime::now(),
        }
    }

    /// [`Self::as_inputs`], with `force`/`wait` cleared — what every internal
    /// re-resolve must use instead (see the field doc above).
    fn as_reresolve_inputs(&self) -> Inputs<'_> {
        Inputs {
            cwd: &self.cwd,
            env: &self.env,
            slot: self.slot.clone(),
            force: false,
            wait: false,
            allow_exec: self.allow_exec,
            now: std::time::SystemTime::now(),
        }
    }

    /// The `path` every watch of this resolution is opened at.
    fn path(&self) -> Option<&str> {
        match &self.slot {
            Slot::Cwd => Some(&self.cwd),
            Slot::Path(p) => Some(p),
            Slot::Global => None,
        }
    }
}

/// The nodes a resolution read, by `(key, path)`, `None` for a miss: what a
/// frame on that subscription is compared with.
type Read = HashMap<(String, Option<String>), Option<Got>>;

/// A [`Fetch`] that keeps the nodes it fetched.
struct Recording<'f> {
    fetch: &'f mut dyn Fetch,
    read: Read,
}

impl Fetch for Recording<'_> {
    fn get(
        &mut self,
        key: &str,
        path: Option<&str>,
        force: bool,
        wait: bool,
    ) -> Result<Option<Got>, FetchError> {
        let got = self.fetch.get(key, path, force, wait)?;
        self.read
            .insert((key.to_string(), path.map(str::to_string)), got.clone());
        Ok(got)
    }
}

/// What one resolution of the watched key came to.
struct Resolution {
    value: Option<JsonValue>,
    deps: Vec<(String, Option<String>)>,
    read: Read,
}

/// `resolve_traced` over `fetch`, keeping what it read.
fn resolve_reading(
    key: &str,
    inputs: &Inputs<'_>,
    fetch: &mut dyn Fetch,
) -> Result<Resolution, WatchError> {
    let mut recording = Recording {
        fetch,
        read: Read::new(),
    };
    let traced = resolve_traced(key, inputs, &mut recording).map_err(watch_error)?;
    Ok(Resolution {
        value: traced.value,
        deps: traced.deps,
        read: recording.read,
    })
}

/// Whether a frame's node is the one a resolution read: the same data,
/// template and rest. Ages are not compared; a frame is about what changed.
fn already_read(read: Option<&Option<Got>>, frame: &Got) -> bool {
    let Some(read) = read else {
        return false;
    };
    let miss = Got::default();
    let read = read.as_ref().unwrap_or(&miss);
    read.data == frame.data && read.template == frame.template && read.rest == frame.rest
}

/// One daemon watch the fan-in holds: on `(key, path)`, a key the latest
/// resolution read.
struct Subscription {
    key: String,
    path: Option<String>,
    stream: WatchStream,
    /// Whether the daemon's opening frame, its answer to the subscription,
    /// has been taken. Opening a subscription does not wait for it.
    opened: bool,
}

/// A watch over every key one resolution reads.
///
/// Open it with [`ResolvedWatch::open`] or
/// [`Client::watch_resolved`](crate::Client::watch_resolved), then call
/// [`next`](Self::next) until it reports [`Eof`](WatchOutcome::Eof). The
/// connections are held open for as long as this value lives; drop it to
/// disconnect.
pub struct ResolvedWatch<'a> {
    client: ClientRef<'a>,
    inputs: OwnedInputs,
    key: String,
    kind: Kind,
    /// One open daemon watch per subscribed `(key, path)`.
    streams: Vec<Subscription>,
    /// The value last emitted, which is what a new resolution is compared to.
    last: Option<JsonValue>,
    deps: Vec<(String, Option<String>)>,
    /// What the latest resolution read at each dep.
    read: Read,
    cancel: CancelHandle,
    /// Whether the resolution `open` already did is still to be emitted.
    pending_initial: bool,
}

impl<'a> ResolvedWatch<'a> {
    /// Open a watch on `key`, resolved as `inputs` describes.
    ///
    /// A provider key is resolved once here, so the first
    /// [`next`](Self::next) answers with the current value without waiting for
    /// a change.
    pub fn open(client: &'a Client, key: &str, inputs: &Inputs<'_>) -> Result<Self, WatchError> {
        Self::open_with(ClientRef::Borrowed(client), key, inputs)
    }

    fn open_with(
        client: ClientRef<'a>,
        key: &str,
        inputs: &Inputs<'_>,
    ) -> Result<Self, WatchError> {
        if namespace(key) == "env" {
            return Err(WatchError::EnvKey("env keys cannot be watched".to_string()));
        }
        let cancel =
            CancelHandle::new().map_err(|e| WatchError::Transport(CombError::IoError(e)))?;
        let inputs = OwnedInputs::new(inputs);
        let path = inputs.path().map(str::to_string);

        // A facet key is already inside a facet: it has no template half, so
        // there is nothing to render and nothing to re-read on a frame.
        if is_facet_key(key) {
            let mut watch = Self {
                client,
                inputs,
                key: key.to_string(),
                kind: Kind::Raw,
                streams: Vec::new(),
                last: None,
                deps: Vec::new(),
                read: Read::new(),
                cancel,
                // The first frame the daemon sends is the current value.
                pending_initial: false,
            };
            watch.follow(vec![(key.to_string(), path)])?;
            return Ok(watch);
        }

        let mut session = client.session().map_err(WatchError::Transport)?;
        let opened = resolve_reading(key, &inputs.as_inputs(), &mut session)?;

        let mut watch = Self {
            client,
            inputs,
            key: key.to_string(),
            kind: Kind::Resolved(Some(session)),
            streams: Vec::new(),
            last: opened.value,
            deps: Vec::new(),
            read: opened.read,
            cancel,
            pending_initial: true,
        };
        // A change landing between the resolve above and a subscription the
        // daemon has not yet made is in that subscription's opening frame,
        // which differs from what the resolve read: `next` re-resolves on it,
        // so the change is emitted rather than lost, and nothing here waits
        // for the daemon.
        watch.follow(opened.deps)?;
        Ok(watch)
    }

    /// Block up to `timeout` (`None` blocks until something happens) for the
    /// next value. The first call on a provider key answers with the value as
    /// it was when the watch opened.
    ///
    /// Only a change is emitted: a frame whose re-resolution comes out the
    /// same value is not one, so a template edited to another spelling of the
    /// same answer, or a poll that found nothing new, keeps the wait going.
    ///
    /// The wait is one `poll(2)` over every open stream and the
    /// [`cancel_handle`](Self::cancel_handle)'s pipe, so it ends the moment a
    /// frame lands on any stream or the handle is cancelled, and a finite
    /// `timeout` returns [`Timeout`](WatchOutcome::Timeout) once it has passed,
    /// not before. `Some(Duration::ZERO)` is a poll: frames already queued are
    /// still seen, and `Timeout` answers only when there are none.
    ///
    /// On a facet key, a frame the daemon sent before it closed the
    /// connection — its refusal of the key, say — is answered before the end
    /// of the stream. Ends with [`Eof`](WatchOutcome::Eof), not a hang, once
    /// nothing is left that could ever answer — the daemon closing a stream,
    /// or a re-resolve dropping the last of them.
    pub fn next(&mut self, timeout: Option<Duration>) -> Result<WatchOutcome, WatchError> {
        if self.pending_initial {
            self.pending_initial = false;
            return Ok(WatchOutcome::Value(self.last.clone()));
        }
        // `checked_add`: a timeout large enough to overflow `Instant`
        // arithmetic is treated as no deadline at all (effectively forever)
        // rather than panicking.
        let deadline = timeout.and_then(|t| Instant::now().checked_add(t));
        // Frames keep the wait from timing out only until one wait has been
        // made: a stream of frames that change nothing must not hold a
        // finite `next` past its deadline.
        let mut waited = false;
        loop {
            if self.cancel.is_cancelled() {
                return Ok(WatchOutcome::Cancelled);
            }
            if let Some(outcome) = self.take_buffered()? {
                return Ok(outcome);
            }
            if self.streams.is_empty() {
                // Nothing left that can ever fire.
                return Ok(WatchOutcome::Eof);
            }
            if waited && deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(WatchOutcome::Timeout);
            }
            let fds: Vec<RawFd> = self.streams.iter().map(|s| s.stream.as_raw_fd()).collect();
            match wait_readable(&fds, Some(self.cancel.wake_fd()), deadline)
                .map_err(|e| WatchError::Transport(CombError::IoError(e)))?
            {
                Ready::Woken => return Ok(WatchOutcome::Cancelled),
                Ready::TimedOut => return Ok(WatchOutcome::Timeout),
                Ready::Streams(ready) => {
                    for index in ready {
                        self.streams[index].stream.read_ready();
                    }
                }
            }
            waited = true;
        }
    }

    /// The handle that ends a wait. [`CancelHandle::cancel`] — from this
    /// thread or another — makes the current or next [`next`](Self::next)
    /// answer [`Cancelled`](WatchOutcome::Cancelled) at once.
    pub fn cancel_handle(&self) -> CancelHandle {
        self.cancel.clone()
    }

    /// The `(key, path)` pairs the latest resolution read: what this watch is
    /// subscribed to, plus any key the daemon refused, which stays listed
    /// because a `put` can make it answerable later. A facet key is its own
    /// single dep.
    pub fn deps(&self) -> &[(String, Option<String>)] {
        &self.deps
    }

    /// The frames already read, which `poll` cannot see, taken before any
    /// wait: what they come to, or `None` to wait for more.
    ///
    /// On a facet key a frame is the value. On a provider key a frame is a
    /// change notice unless it shows the node the latest resolution read
    /// there, which that resolution has already answered for: every frame
    /// buffered is taken, then one re-resolve answers for all the notices. A
    /// stream's opening frame is read the same way — a change between the
    /// resolve and the subscription shows in it — unless it is the daemon's
    /// refusal of the key: that stream is dropped, and the key stays in
    /// [`Self::deps`] — a `put` can make it answerable later, and the next
    /// re-resolve subscribes it again. A stream the daemon has closed ends the watch: once its frames
    /// are taken on a facet key, and at once on a provider key, whose notices
    /// only a re-resolve against that daemon could act on.
    fn take_buffered(&mut self) -> Result<Option<WatchOutcome>, WatchError> {
        if let Kind::Raw = self.kind {
            let Some(raw) = self.streams.first_mut() else {
                return Ok(Some(WatchOutcome::Eof));
            };
            return match raw.stream.take_frame() {
                Some(Ok(frame)) => Ok(Some(WatchOutcome::Value(frame_value(&frame)?))),
                // A frame that is not text is the connection ending, as any
                // other read failure is.
                Some(Err(_)) => Ok(Some(WatchOutcome::Eof)),
                None => Ok(raw.stream.is_closed().then_some(WatchOutcome::Eof)),
            };
        }

        let mut changed = false;
        let mut ended = false;
        let read = &self.read;
        self.streams.retain_mut(|sub| {
            while let Some(frame) = sub.stream.take_frame() {
                let Ok(frame) = frame else {
                    ended = true;
                    return true;
                };
                let opening = !std::mem::replace(&mut sub.opened, true);
                match crate::envelope(&frame, None).map(crate::parse_node) {
                    Err(_) if opening => return false,
                    Ok(node)
                        if already_read(read.get(&(sub.key.clone(), sub.path.clone())), &node) => {}
                    _ => changed = true,
                }
            }
            ended |= sub.stream.is_closed();
            true
        });
        // A notice is a reason to re-resolve, not a value to deliver; with a
        // stream closed the daemon is going or gone, and a re-resolve would
        // only fail on it.
        if ended {
            return Ok(Some(WatchOutcome::Eof));
        }
        if changed && self.reresolve()? {
            return Ok(Some(WatchOutcome::Value(self.last.clone())));
        }
        Ok(None)
    }

    /// Resolve again, move the subscriptions onto that resolution's deps, and
    /// report whether the value changed.
    fn reresolve(&mut self) -> Result<bool, WatchError> {
        let Kind::Resolved(held) = &mut self.kind else {
            unreachable!("only a provider key resolves");
        };
        let mut session = match held.take() {
            Some(session) => session,
            None => self.client.session().map_err(WatchError::Transport)?,
        };
        let again = resolve_reading(&self.key, &self.inputs.as_reresolve_inputs(), &mut session)?;
        *held = Some(session);
        self.read = again.read;
        self.follow(again.deps)?;

        if again.value == self.last {
            return Ok(false);
        }
        self.last = again.value;
        Ok(true)
    }

    /// Subscribe to `deps` and nothing else: a key the resolution no longer
    /// reads is dropped, a key it has started reading is opened. Opening one
    /// sends the request and waits for nothing; its opening frame is taken
    /// when it arrives.
    fn follow(&mut self, deps: Vec<(String, Option<String>)>) -> Result<(), WatchError> {
        self.streams.retain(|sub| {
            deps.iter()
                .any(|(key, path)| *key == sub.key && *path == sub.path)
        });
        for (key, path) in &deps {
            if self
                .streams
                .iter()
                .any(|sub| sub.key == *key && sub.path == *path)
            {
                continue;
            }
            let stream = self
                .client
                .watch(key, path.as_deref())
                .map_err(WatchError::Transport)?;
            self.streams.push(Subscription {
                key: key.clone(),
                path: path.clone(),
                stream,
                opened: false,
            });
        }
        self.deps = deps;
        Ok(())
    }
}

impl ResolvedWatch<'static> {
    /// [`open`](Self::open), over a client this watch owns a share of.
    ///
    /// The watch borrows nothing, so it can be stored in a handle that
    /// outlives every stack frame in sight — which is what a `bc_watch_*`
    /// handle is. The [`Arc`] is what keeps the client alive; there is no
    /// "the client must outlive the watch" contract for a caller to get
    /// wrong.
    pub fn open_owned(
        client: Arc<Client>,
        key: &str,
        inputs: &Inputs<'_>,
    ) -> Result<Self, WatchError> {
        Self::open_with(ClientRef::Owned(client), key, inputs)
    }
}

/// A facet key: a colon in the key's head, the syntax
/// [`crate::key::parse_facet`] reads. The daemon judges whether it is a
/// well-formed one.
fn is_facet_key(key: &str) -> bool {
    namespace(key).contains(':')
}

/// The head of a key: everything before the first `.` or `[`.
fn namespace(key: &str) -> &str {
    crate::key::head_of(key)
}

/// The value a raw frame carries, or the daemon's refusal of the key.
fn frame_value(frame: &str) -> Result<Option<JsonValue>, WatchError> {
    WatchStream::parse_line(frame)
        .map(|event| event.data.map(|data| data.as_value().clone()))
        .map_err(WatchError::Transport)
}

/// A resolution failure as a watch reports it: a fetch that could not reach
/// the daemon is the transport's, with the error behind it.
fn watch_error(e: ResolveError) -> WatchError {
    match e {
        ResolveError::Fetch(FetchError::Transport(transport)) => WatchError::Transport(transport),
        e => WatchError::Resolve(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(force: bool, wait: bool) -> OwnedInputs {
        OwnedInputs::new(&Inputs {
            cwd: "/",
            env: &HashMap::new(),
            slot: Slot::Global,
            force,
            wait,
            allow_exec: false,
            now: std::time::SystemTime::now(),
        })
    }

    /// Pins item 3: `force`/`wait` are opening instructions.
    /// [`OwnedInputs::as_inputs`] (the resolve `open` itself performs) keeps
    /// them, but [`OwnedInputs::as_reresolve_inputs`] (every internal
    /// re-resolve) must clear them, or a forced watch on a
    /// per-execution-changing provider would re-execute it, and so re-fire
    /// itself, on every frame.
    #[test]
    fn reresolve_inputs_clear_force_and_wait_but_open_inputs_keep_them() {
        let owned = owned(true, true);

        let opening = owned.as_inputs();
        assert!(opening.force, "the opening resolve keeps force");
        assert!(opening.wait, "the opening resolve keeps wait");

        let reresolve = owned.as_reresolve_inputs();
        assert!(!reresolve.force, "a re-resolve must not replay force");
        assert!(!reresolve.wait, "a re-resolve must not replay wait");
    }

    /// Pins item 1: a watch with nothing left that could ever fire — no dep
    /// streams — ends with `Eof` rather than
    /// busy-spinning the deadline check that used to live only inside the
    /// per-stream loop (which never ran at zero streams). No daemon needed:
    /// the fix returns before any stream is ever touched.
    #[test]
    fn next_ends_when_no_streams_remain() {
        let client = Client::new();
        let mut w = ResolvedWatch {
            client: ClientRef::Borrowed(&client),
            inputs: owned(false, false),
            key: "p.field".to_string(),
            kind: Kind::Resolved(None),
            streams: Vec::new(),
            last: None,
            deps: Vec::new(),
            read: Read::new(),
            cancel: CancelHandle::new().expect("a cancel handle"),
            pending_initial: false,
        };
        match w.next(Some(Duration::from_millis(50))) {
            Ok(WatchOutcome::Eof) => {}
            other => panic!("expected Eof with nothing left to poll, got {other:?}"),
        }
    }

    /// `n` connected socket pairs: the first of each is what a wait polls,
    /// the second what a test writes to or closes.
    fn pairs(n: usize) -> Vec<(UnixStream, UnixStream)> {
        (0..n)
            .map(|_| UnixStream::pair().expect("a pair"))
            .collect()
    }

    fn polled(pairs: &[(UnixStream, UnixStream)]) -> Vec<RawFd> {
        pairs.iter().map(|(polled, _)| polled.as_raw_fd()).collect()
    }

    /// A count that grows by one each time a thread of this process goes
    /// back to sleep in the kernel. On Linux it is the voluntary context
    /// switches (`getrusage`'s `ru_nvcsw`). macOS leaves that field 0 and
    /// counts no voluntary switches anywhere (`task_info`'s `csw` adds the
    /// preemptions a loaded machine makes), so there it is the BSD system
    /// calls made (`syscalls_unix`): a thread that wakes from a wait must make
    /// another to sleep again. Nextest runs each test in a process of its
    /// own, so a test's delta is its own.
    #[cfg(target_os = "linux")]
    fn sleeps() -> i64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: `usage` is a writable `rusage` for the call to fill.
        assert_eq!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
            0
        );
        // SAFETY: `getrusage` succeeded, so it filled `usage`.
        i64::from(unsafe { usage.assume_init() }.ru_nvcsw)
    }

    #[cfg(target_os = "macos")]
    fn sleeps() -> i64 {
        /// `task_events_info`, `mach/task_info.h`; the libc crate binds
        /// `task_info` but not this flavour's struct. The kernel fills every
        /// field; one is read.
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
        let mut count = (std::mem::size_of::<TaskEventsInfo>()
            / std::mem::size_of::<libc::natural_t>())
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

    /// A byte on the last of twenty streams is seen at once: one `poll`
    /// covers them all, so nothing waits on the nineteen before it.
    #[test]
    fn a_byte_on_the_last_of_twenty_streams_is_seen_at_once() {
        let pairs = pairs(20);
        (&pairs[19].1).write_all(b"x").expect("write");

        let start = Instant::now();
        let ready = wait_readable(&polled(&pairs), None, Some(start + Duration::from_secs(5)))
            .expect("wait");
        let took = start.elapsed();
        assert_eq!(ready, Ready::Streams(vec![19]));
        assert!(took < Duration::from_millis(5), "{took:?}");
    }

    /// A peer that hung up is readable: the read that follows finds the end.
    #[test]
    fn a_closed_peer_is_ready_to_read() {
        let pairs = pairs(2);
        let (closed, peer) = UnixStream::pair().expect("a pair");
        drop(peer);
        let mut fds = polled(&pairs);
        fds.push(closed.as_raw_fd());
        assert_eq!(
            wait_readable(&fds, None, Some(Instant::now() + Duration::from_secs(5))).expect("wait"),
            Ready::Streams(vec![2])
        );
    }

    /// An idle wait ends at its deadline, never before it and promptly after,
    /// and it sleeps once: no wake per stream per tick. Twenty idle streams
    /// for 100 ms cost at most three sleeps.
    #[test]
    fn an_idle_wait_times_out_at_its_deadline_and_sleeps_until_then() {
        let pairs = pairs(20);
        let cancel = CancelHandle::new().expect("a cancel handle");

        let before = sleeps();
        let start = Instant::now();
        let ready = wait_readable(
            &polled(&pairs),
            Some(cancel.wake_fd()),
            Some(start + Duration::from_millis(100)),
        )
        .expect("wait");
        let took = start.elapsed();
        let slept = sleeps() - before;

        // Never before the deadline; after it, within a margin a loaded
        // machine's scheduling needs and a 50 ms tick would not meet.
        // `benches/watch.rs` measures the overshoot itself.
        assert_eq!(ready, Ready::TimedOut);
        assert!(
            took >= Duration::from_millis(100) && took < Duration::from_millis(120),
            "{took:?}"
        );
        assert!(slept <= 3, "{slept} sleeps");
    }

    /// A cancel from another thread ends a wait at once, and every wait after
    /// it: the pipe's byte is never read.
    #[test]
    fn a_cancel_from_another_thread_ends_the_wait_at_once() {
        let pairs = pairs(20);
        let cancel = CancelHandle::new().expect("a cancel handle");
        let (go, told) = std::sync::mpsc::channel::<()>();
        let canceller = {
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                told.recv().expect("told to cancel");
                let at = Instant::now();
                cancel.cancel();
                at
            })
        };

        go.send(()).expect("tell the canceller");
        let ready = wait_readable(
            &polled(&pairs),
            Some(cancel.wake_fd()),
            Some(Instant::now() + Duration::from_secs(5)),
        )
        .expect("wait");
        let woke = Instant::now();
        let cancelled_at = canceller.join().expect("the canceller");

        assert_eq!(ready, Ready::Woken);
        // A tick-driven wait would take up to a tick (50 ms); this one wakes
        // as the thread is scheduled. The bound leaves a loaded machine room
        // for that; `benches/watch.rs` measures the latency itself.
        let after = woke.saturating_duration_since(cancelled_at);
        assert!(after < Duration::from_millis(20), "{after:?}");
        assert!(cancel.is_cancelled());
        assert_eq!(
            wait_readable(
                &polled(&pairs),
                Some(cancel.wake_fd()),
                Some(Instant::now())
            )
            .expect("wait"),
            Ready::Woken,
            "a cancel ends every later wait too"
        );
    }
}
