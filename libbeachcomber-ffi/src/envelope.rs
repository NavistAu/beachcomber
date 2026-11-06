//! JSON envelope construction and the panic boundary.
//!
//! Every `extern "C"` entry point in this crate routes its body through
//! [`call_ffi`], which catches panics (unwinding across an FFI boundary is
//! undefined behaviour) and always returns a freeable, NUL-terminated JSON
//! string of one of two shapes:
//!
//! ```json
//! {"ok": true,  "data": <op result>}
//! {"ok": false, "error": {"kind": "...", "message": "..."}}
//! ```

use std::ffi::CString;
use std::os::raw::c_char;
use std::panic::{self, UnwindSafe};

use libbeachcomber::CombError;
use libbeachcomber::resolve::{FetchError, ResolveError};
use libbeachcomber::watch::WatchError;
use serde::Serialize;
use serde_json::json;

/// The stable, machine-readable error kinds: every [`CombError::kind`] slug,
/// plus the conditions the ABI itself raises.
pub mod kind {
    pub use libbeachcomber::kind::*;

    /// The handle's connection is in use by another caller.
    pub const BUSY: &str = "busy";
    /// The wrapped call panicked; caught at the FFI boundary.
    pub const PANIC: &str = "panic";
}

/// An error to be enveloped: a stable [`kind`] slug plus a human-readable
/// message.
#[derive(Debug, Clone)]
pub struct FfiError {
    pub kind: &'static str,
    pub message: String,
}

impl FfiError {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// A library failure, kind and all ([`CombError::kind`]). A server error's
/// message is the daemon's words alone — the same text `comb` prints — since
/// `kind` already says whose words they are. Every other variant's Display
/// names its condition.
impl From<CombError> for FfiError {
    fn from(e: CombError) -> Self {
        let kind = e.kind();
        let message = match e {
            CombError::ServerError(s) => s,
            other => other.to_string(),
        };
        FfiError::new(kind, message)
    }
}

/// A resolution failure, classified as every surface classifies it
/// ([`ResolveError::kind`]); a transport failure is its `CombError`.
impl From<ResolveError> for FfiError {
    fn from(e: ResolveError) -> Self {
        match e {
            ResolveError::Fetch(FetchError::Transport(t)) => t.into(),
            e => FfiError::new(e.kind(), e.to_string()),
        }
    }
}

/// A watch failure ([`WatchError::kind`]): a resolution's or a transport's
/// as those are, and a key that cannot be watched at all in the library's
/// own words — no daemon was involved in deciding it.
impl From<WatchError> for FfiError {
    fn from(e: WatchError) -> Self {
        match e {
            WatchError::Resolve(e) => e.into(),
            WatchError::Transport(e) => e.into(),
            e => FfiError::new(e.kind(), e.to_string()),
        }
    }
}

fn ok_json(data: impl Serialize) -> String {
    json!({ "ok": true, "data": data }).to_string()
}

fn err_json(err: &FfiError) -> String {
    json!({
        "ok": false,
        "error": { "kind": err.kind, "message": err.message },
    })
    .to_string()
}

/// A `bc_watch_next` result. Unlike every other op, a watch poll has more
/// than one non-error outcome a binding must tell apart without string
/// matching — an event arrived, the wait elapsed, the daemon closed the
/// stream, or [`bc_watch_cancel`] was called — so each is a distinct,
/// machine-readable envelope shape rather than folding into the ordinary
/// `data` field. The fifth outcome, `error`, is the existing `ok: false`
/// envelope: it was already machine-readable via `error.kind` and needs no
/// new shape.
///
/// [`bc_watch_cancel`]: crate::bc_watch_cancel
pub enum WatchOutcome {
    /// `{"ok": true, "outcome": "event", "data": <event>}`
    Event(serde_json::Value),
    /// `{"ok": true, "outcome": "timeout"}`
    Timeout,
    /// `{"ok": true, "outcome": "eof"}`
    Eof,
    /// `{"ok": true, "outcome": "cancelled"}`
    Cancelled,
}

fn watch_outcome_json(outcome: WatchOutcome) -> String {
    match outcome {
        WatchOutcome::Event(data) => json!({ "ok": true, "outcome": "event", "data": data }),
        WatchOutcome::Timeout => json!({ "ok": true, "outcome": "timeout" }),
        WatchOutcome::Eof => json!({ "ok": true, "outcome": "eof" }),
        WatchOutcome::Cancelled => json!({ "ok": true, "outcome": "cancelled" }),
    }
    .to_string()
}

fn into_c_string(body: String) -> *mut c_char {
    // `body` is our own serialized JSON; it cannot contain an interior NUL.
    CString::new(body)
        .expect("serialized JSON envelope must not contain a NUL byte")
        .into_raw()
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// The single wrapper every `extern "C"` entry point routes through.
///
/// `f` produces the op's JSON result on success. Panics inside `f` are
/// caught and turned into a `kind: "panic"` envelope rather than unwinding
/// across the ABI boundary. The returned pointer is always non-null and must
/// be freed with [`bc_string_free`].
pub fn call_ffi<F>(f: F) -> *mut c_char
where
    F: FnOnce() -> Result<serde_json::Value, FfiError> + UnwindSafe,
{
    let body = match panic::catch_unwind(f) {
        Ok(Ok(data)) => ok_json(data),
        Ok(Err(err)) => err_json(&err),
        Err(payload) => err_json(&FfiError::new(kind::PANIC, panic_message(&*payload))),
    };
    into_c_string(body)
}

/// Like [`call_ffi`], but for `bc_watch_next`'s five-outcome envelope (see
/// [`WatchOutcome`]) rather than the ordinary `{"ok":true,"data":...}`
/// shape. Panics inside `f` are caught the same way; the returned pointer
/// is always non-null and must be freed with [`bc_string_free`].
pub fn call_watch_next<F>(f: F) -> *mut c_char
where
    F: FnOnce() -> Result<WatchOutcome, FfiError> + UnwindSafe,
{
    let body = match panic::catch_unwind(f) {
        Ok(Ok(outcome)) => watch_outcome_json(outcome),
        Ok(Err(err)) => err_json(&err),
        Err(payload) => err_json(&FfiError::new(kind::PANIC, panic_message(&*payload))),
    };
    into_c_string(body)
}

/// Frees a string returned by any `bc_*` function that documents its result
/// as caller-owned. Null-safe. Never call this on `bc_version()`'s return
/// value — that string is static and not owned by the caller.
///
/// # Safety
/// `ptr` must be either NULL or a pointer previously returned by one of this
/// crate's `char *`-returning functions, not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_string_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    drop(unsafe { CString::from_raw(ptr) });
}
