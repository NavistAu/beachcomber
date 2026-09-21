//! The watch fan-in: one stream of the values `comb get` would print.
//!
//! A raw daemon watch answers about one address. What a caller asked to watch
//! is a *value*, and a value is what [`resolve`](crate::resolve) makes of
//! every key it reads — the addressed one, the refs its template names, and
//! the template tree itself, any of which can change under it. So this module
//! opens one daemon watch per key that resolution read plus one on the
//! provider's `<p>:template`, polls them in turn, re-resolves on any frame and
//! emits only when the resolved value changed (design §5.5).
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

use crate::resolve::{FetchError, Inputs, ResolveError, Slot, resolve_with_deps};
use crate::{Client, ClientFetch, CombError, WatchStream};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The longest one read of one stream waits before the fan-in moves on.
///
/// It bounds two things: how long a wait can outlive
/// [`ResolvedWatch::cancel_handle`] being set on another thread, and how far
/// past its deadline a [`ResolvedWatch::next`] can run.
const POLL_TICK: Duration = Duration::from_millis(50);

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
    client: &'a Client,
    inputs: OwnedInputs,
    key: String,
    kind: Kind,
    /// One open daemon watch per subscribed `(key, path)`.
    streams: Vec<(String, Option<String>, WatchStream)>,
    /// The watch on `<p>:template`; a facet key has none.
    template_stream: Option<WatchStream>,
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
        if key == "env" || key.starts_with("env.") {
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
                template_stream: None,
                last: None,
                deps: vec![(key.to_string(), path)],
                cancelled: Arc::new(AtomicBool::new(false)),
                // The first frame the daemon sends is the current value.
                pending_initial: false,
            });
        }

        let mut fetch = ClientFetch::new(client);
        let resolved = {
            let borrowed = inputs.as_inputs();
            resolve_with_deps(key, &borrowed, &mut fetch)
        };
        let (last, deps) = resolved.map_err(|e| watch_error(e, &mut fetch))?;

        let mut streams = Vec::new();
        for (dep_key, dep_path) in &deps {
            if let Some(stream) = open_stream(client, dep_key, dep_path.as_deref())? {
                streams.push((dep_key.clone(), dep_path.clone(), stream));
            }
        }
        // The template tree is watched whether or not the key has one today:
        // a `put` can give it one while this watch is open.
        let provider = namespace(key);
        let template_stream =
            open_stream(client, &format!("{provider}:template"), path.as_deref())?;

        Ok(Self {
            client,
            inputs,
            key: key.to_string(),
            kind: Kind::Resolved,
            streams,
            template_stream,
            last,
            deps,
            cancelled: Arc::new(AtomicBool::new(false)),
            pending_initial: true,
        })
    }

    /// Block up to `timeout` (`None` blocks until something happens) for the
    /// next value. The first call on a provider key answers with the value as
    /// it was when the watch opened.
    ///
    /// Only a change is emitted: a frame whose re-resolution comes out the
    /// same value is not one, so a template edited to another spelling of the
    /// same answer, or a poll that found nothing new, keeps the wait going.
    pub fn next(&mut self, timeout: Option<Duration>) -> Result<WatchOutcome, WatchError> {
        if self.pending_initial {
            self.pending_initial = false;
            return Ok(WatchOutcome::Value(self.last.clone()));
        }
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            if self.cancelled.load(Ordering::SeqCst) {
                return Ok(WatchOutcome::Cancelled);
            }
            // The count is re-read each time round: a re-resolve can open and
            // drop subscriptions while this loop is walking them.
            let mut index = 0;
            while index < self.stream_count() {
                let Some(tick) = tick(deadline) else {
                    return Ok(WatchOutcome::Timeout);
                };
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
        self.streams.len() + usize::from(self.template_stream.is_some())
    }

    /// The `index`th stream: the subscriptions in order, the template watch
    /// last. `index` must be below [`Self::stream_count`].
    fn stream(&mut self, index: usize) -> &mut WatchStream {
        if index < self.streams.len() {
            &mut self.streams[index].2
        } else {
            self.template_stream
                .as_mut()
                .expect("index is below stream_count")
        }
    }

    /// Resolve again, move the subscriptions onto that resolution's deps, and
    /// report whether the value changed.
    fn reresolve(&mut self) -> Result<bool, WatchError> {
        let mut fetch = ClientFetch::new(self.client);
        let resolved = {
            let inputs = self.inputs.as_inputs();
            resolve_with_deps(&self.key, &inputs, &mut fetch)
        };
        let (value, deps) = resolved.map_err(|e| watch_error(e, &mut fetch))?;

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
            if let Some(stream) = open_stream(self.client, dep_key, dep_path.as_deref())? {
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

/// A facet key: a colon in the key's first segment, the syntax
/// `beachcomber::query::parse_facet` reads. The daemon judges whether it is a
/// well-formed one.
fn is_facet_key(key: &str) -> bool {
    namespace(key).contains(':')
}

/// The namespace segment of a key: everything before the first dot.
fn namespace(key: &str) -> &str {
    key.split('.').next().unwrap_or(key)
}

/// One daemon watch, with its first frame taken off the wire.
///
/// The daemon answers a watch it refuses with one error frame, so that is
/// where a refusal shows up: the stream is dropped (`Ok(None)`) while the key
/// it names stays in [`ResolvedWatch::deps`] — a `put` can make it answerable
/// later, and the template watch fires then. Taking the frame is also what
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

/// A resolution failure as a watch reports it. A fetch that could not reach
/// the daemon is the transport's, and [`ClientFetch`] kept the error behind
/// it, so a caller classifying failures by type reads it rather than parsing
/// a message back.
fn watch_error(e: ResolveError, fetch: &mut ClientFetch<'_>) -> WatchError {
    match (&e, fetch.transport.take()) {
        (ResolveError::Fetch(FetchError::Transport(_)), Some(transport)) => {
            WatchError::Transport(transport)
        }
        _ => WatchError::Resolve(e),
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
