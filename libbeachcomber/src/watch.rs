//! The watch fan-in: one stream of the values `comb get` would print.
//!
//! A raw daemon watch answers about one address. What a caller asked to watch
//! is a *value*, and a value is what [`resolve`](crate::resolve) makes of
//! every key it reads — the addressed one, the refs its template names, and
//! the template tree itself, any of which can change under it. So this module
//! opens one daemon watch per key that resolution read, polls them in turn,
//! re-resolves on any frame and emits only when the resolved value changed
//! (design §5.5). The daemon notifies a namespace's watchers on every write
//! to it or to one of its facets, so a template edit wakes the key's own watch.
//!
//! Three key classes, decided at [`ResolvedWatch::open`]:
//!
//! - `env` / `env.X` — constant for the process, so there is nothing to
//!   watch: [`WatchError::EnvKey`].
//! - a facet key, `p:facet.rest` — no template half, so no resolution: one
//!   plain daemon watch whose frames are emitted as they arrive.
//! - a provider key, `p.rest` — the fan-in.
//!
//! The library has no runtime, so the fan-in is synchronous: each stream is
//! read with a short timeout in turn, which is also what lets
//! [`ResolvedWatch::cancel_handle`] end a wait from another thread.

use crate::resolve::{FetchError, Inputs, ResolveError, Slot, resolve_traced};
use crate::{Client, CombError, WatchStream};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The longest one read of one stream waits before the fan-in moves on.
///
/// It bounds two things: how long a wait can outlive
/// [`ResolvedWatch::cancel_handle`] being set on another thread, and how far
/// past its deadline a [`ResolvedWatch::next`] can run.
const POLL_TICK: Duration = Duration::from_millis(50);

/// The read attempted when the deadline is already spent but `next` still
/// owes the current pass a read — a `Some(Duration::ZERO)` poll, or a
/// deadline that elapsed between two streams of the same pass.
const MIN_TICK: Duration = Duration::from_millis(1);

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
    /// A provider key: the fan-in, re-resolved on every frame.
    Resolved,
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
    streams: Vec<(String, Option<String>, WatchStream)>,
    /// The value last emitted, which is what a new resolution is compared to.
    last: Option<JsonValue>,
    deps: Vec<(String, Option<String>)>,
    cancelled: Arc<AtomicBool>,
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
        let inputs = OwnedInputs::new(inputs);
        let path = inputs.path().map(str::to_string);

        // A facet key is already inside a facet: it has no template half, so
        // there is nothing to render and nothing to re-read on a frame.
        if is_facet_key(key) {
            let stream = client
                .watch(key, path.as_deref())
                .map_err(WatchError::Transport)?;
            return Ok(Self {
                client,
                inputs,
                key: key.to_string(),
                kind: Kind::Raw,
                streams: vec![(key.to_string(), path.clone(), stream)],
                last: None,
                deps: vec![(key.to_string(), path)],
                cancelled: Arc::new(AtomicBool::new(false)),
                // The first frame the daemon sends is the current value.
                pending_initial: false,
            });
        }

        let mut fetch: &Client = &client;
        let resolved = {
            let borrowed = inputs.as_inputs();
            resolve_traced(key, &borrowed, &mut fetch).map(|t| (t.value, t.deps))
        };
        let (last, deps) = resolved.map_err(watch_error)?;

        let mut streams = Vec::new();
        for (dep_key, dep_path) in &deps {
            if let Some(stream) = open_stream(&client, dep_key, dep_path.as_deref())? {
                streams.push((dep_key.clone(), dep_path.clone(), stream));
            }
        }
        let mut watch = Self {
            client,
            inputs,
            key: key.to_string(),
            kind: Kind::Resolved,
            streams,
            last,
            deps,
            cancelled: Arc::new(AtomicBool::new(false)),
            pending_initial: true,
        };
        // A change landing between the resolve above and the streams just
        // opened is consumed as one of those streams' initial frame rather
        // than ever being emitted — that frame is what seeds subscriptions,
        // not a change `next` reports — so `last` would stay stale until an
        // unrelated later change happened to correct it. Re-resolve once
        // more, now that every stream is open, and reuse `reresolve`'s
        // bookkeeping to fold the result into `last`/`deps` without emitting
        // it; `next`'s pending-initial answer is this final value. This
        // re-read is unforced: it returns what a forced open just
        // materialised in the daemon's cache, which is what keeps `force`
        // at open meaningful.
        watch.reresolve()?;
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
    /// `Some(Duration::ZERO)` is a poll: one non-blocking-ish pass over every
    /// open stream, then `Timeout` if none of them had anything — never a
    /// `Timeout` before that pass runs, so a frame already queued when `next`
    /// is called is still seen. This is the contract the FFI documents for
    /// `timeout_ms == 0` (a single ~1ms attempt).
    ///
    /// The deadline is checked per pass, not per read, so a finite `timeout`
    /// can be overrun by up to one pass: N streams × 1 ms plus a re-resolve
    /// round trip per frame consumed in that pass.
    ///
    /// Ends with [`Eof`](WatchOutcome::Eof), not a hang, once nothing is left
    /// that could ever answer — the daemon closing every stream, or a
    /// re-resolve dropping the last of them.
    pub fn next(&mut self, timeout: Option<Duration>) -> Result<WatchOutcome, WatchError> {
        if self.pending_initial {
            self.pending_initial = false;
            return Ok(WatchOutcome::Value(self.last.clone()));
        }
        // `checked_add`: a timeout large enough to overflow `Instant`
        // arithmetic is treated as no deadline at all (effectively forever)
        // rather than panicking.
        let deadline = timeout.and_then(|t| Instant::now().checked_add(t));
        // At least one full pass over the streams happens before the
        // deadline can end the wait: otherwise `Some(Duration::ZERO)` would
        // expire before a single read, and a watch with zero open streams
        // would busy-spin here instead of ending.
        let mut first_pass = true;
        loop {
            if self.cancelled.load(Ordering::SeqCst) {
                return Ok(WatchOutcome::Cancelled);
            }
            if self.stream_count() == 0 {
                // Nothing left that can ever fire.
                return Ok(WatchOutcome::Eof);
            }
            if !first_pass && tick(deadline).is_none() {
                return Ok(WatchOutcome::Timeout);
            }
            first_pass = false;

            // The count is re-read each time round: a re-resolve can open and
            // drop subscriptions while this loop is walking them.
            let mut index = 0;
            while index < self.stream_count() {
                // A deadline already spent — `Some(ZERO)`'s first pass, or
                // one that elapsed between two streams of the same pass —
                // still gets a minimal read rather than being skipped.
                let tick = tick(deadline).unwrap_or(MIN_TICK);
                let frame = match self.stream(index).read_frame_with_timeout(tick) {
                    Ok(Some(frame)) => frame,
                    Ok(None) => return Ok(WatchOutcome::Eof),
                    // Nothing on this one within the tick: try the next.
                    Err(e) if is_timeout_like(&e) => {
                        index += 1;
                        continue;
                    }
                    // Any other read failure is the connection ending, not
                    // this call failing.
                    Err(_) => return Ok(WatchOutcome::Eof),
                };
                match self.kind {
                    Kind::Raw => return Ok(WatchOutcome::Value(frame_value(&frame)?)),
                    Kind::Resolved => {
                        if self.reresolve()? {
                            return Ok(WatchOutcome::Value(self.last.clone()));
                        }
                        index += 1;
                    }
                }
            }
        }
    }

    /// The flag that ends a wait. Setting it — from this thread or another —
    /// makes the current or next [`next`](Self::next) answer
    /// [`Cancelled`](WatchOutcome::Cancelled) within about one poll tick per
    /// open stream.
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    /// The `(key, path)` pairs the latest resolution read: what this watch is
    /// subscribed to, plus any key the daemon refused, which stays listed
    /// because a `put` can make it answerable later. A facet key is its own
    /// single dep.
    pub fn deps(&self) -> &[(String, Option<String>)] {
        &self.deps
    }

    fn stream_count(&self) -> usize {
        self.streams.len()
    }

    /// The `index`th stream, in subscription order. `index` must be below
    /// [`Self::stream_count`].
    fn stream(&mut self, index: usize) -> &mut WatchStream {
        &mut self.streams[index].2
    }

    /// Resolve again, move the subscriptions onto that resolution's deps, and
    /// report whether the value changed.
    fn reresolve(&mut self) -> Result<bool, WatchError> {
        let mut fetch: &Client = &self.client;
        let resolved = {
            let inputs = self.inputs.as_reresolve_inputs();
            resolve_traced(&self.key, &inputs, &mut fetch).map(|t| (t.value, t.deps))
        };
        let (value, deps) = resolved.map_err(watch_error)?;

        // Subscriptions follow the latest resolution: a key it no longer
        // reads is dropped, a key it has started reading is opened.
        self.streams
            .retain(|(key, path, _)| deps.iter().any(|dep| dep.0 == *key && dep.1 == *path));
        for (dep_key, dep_path) in &deps {
            if self
                .streams
                .iter()
                .any(|(key, path, _)| key == dep_key && path == dep_path)
            {
                continue;
            }
            if let Some(stream) = open_stream(&self.client, dep_key, dep_path.as_deref())? {
                self.streams
                    .push((dep_key.clone(), dep_path.clone(), stream));
            }
        }
        self.deps = deps;

        if value == self.last {
            return Ok(false);
        }
        self.last = value;
        Ok(true)
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

/// One daemon watch, with its first frame taken off the wire.
///
/// The daemon answers a watch it refuses with one error frame, so that is
/// where a refusal shows up: the stream is dropped (`Ok(None)`) while the key
/// it names stays in [`ResolvedWatch::deps`] — a `put` can make it answerable
/// later, and that watch fires then. Taking the frame is also what
/// makes every later frame on the stream a change rather than the
/// subscription's own opening echo.
fn open_stream(
    client: &Client,
    key: &str,
    path: Option<&str>,
) -> Result<Option<WatchStream>, WatchError> {
    let mut stream = client.watch(key, path).map_err(WatchError::Transport)?;
    Ok(match stream.read_frame_with_timeout(POLL_TICK) {
        Ok(Some(frame)) => WatchStream::parse_line(&frame).is_ok().then_some(stream),
        // Closed before it answered: there is nothing to poll.
        Ok(None) => None,
        // Nothing yet. The frame is still coming and costs one re-resolve
        // that emits nothing when it lands.
        Err(e) if is_timeout_like(&e) => Some(stream),
        Err(_) => None,
    })
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

/// How long the next read may wait: a poll tick, or what is left of the
/// deadline. `None` is the deadline having passed.
fn tick(deadline: Option<Instant>) -> Option<Duration> {
    let Some(deadline) = deadline else {
        return Some(POLL_TICK);
    };
    let now = Instant::now();
    (now < deadline).then(|| (deadline - now).min(POLL_TICK))
}

/// A read that ran out its timeout rather than failing.
fn is_timeout_like(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
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
            kind: Kind::Resolved,
            streams: Vec::new(),
            last: None,
            deps: Vec::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
            pending_initial: false,
        };
        match w.next(Some(Duration::from_millis(50))) {
            Ok(WatchOutcome::Eof) => {}
            other => panic!("expected Eof with nothing left to poll, got {other:?}"),
        }
    }
}
