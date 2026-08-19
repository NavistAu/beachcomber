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
use serde::Serialize;
use serde_json::json;

/// Stable, machine-readable error kind. One slug per `CombError` variant
/// plus the FFI-specific conditions the ABI itself can raise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// A reserved flag bit was set.
    BadFlags,
    /// The handle's connection is in use by another caller.
    Busy,
    /// The wrapped call panicked; caught at the FFI boundary.
    Panic,
    /// The daemon's reported version does not match this library's.
    VersionSkew,
    /// `CombError::DaemonNotRunning`.
    DaemonNotRunning,
    /// `CombError::ConnectionFailed`.
    ConnectionFailed,
    /// `CombError::IoError`.
    IoError,
    /// `CombError::ParseError`.
    ParseError,
    /// `CombError::ServerError`.
    ServerError,
    /// `CombError::Timeout`.
    Timeout,
}

impl ErrorKind {
    /// The stable slug serialised into the envelope's `error.kind` field.
    pub fn slug(self) -> &'static str {
        match self {
            ErrorKind::BadFlags => "bad_flags",
            ErrorKind::Busy => "busy",
            ErrorKind::Panic => "panic",
            ErrorKind::VersionSkew => "version_skew",
            ErrorKind::DaemonNotRunning => "daemon_not_running",
            ErrorKind::ConnectionFailed => "connection_failed",
            ErrorKind::IoError => "io_error",
            ErrorKind::ParseError => "parse_error",
            ErrorKind::ServerError => "server_error",
            ErrorKind::Timeout => "timeout",
        }
    }
}

/// An error to be enveloped: a stable [`ErrorKind`] plus a human-readable
/// message.
#[derive(Debug, Clone)]
pub struct FfiError {
    pub kind: ErrorKind,
    pub message: String,
}

impl FfiError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl From<CombError> for FfiError {
    fn from(e: CombError) -> Self {
        let kind = match &e {
            CombError::DaemonNotRunning => ErrorKind::DaemonNotRunning,
            CombError::ConnectionFailed(_) => ErrorKind::ConnectionFailed,
            CombError::IoError(_) => ErrorKind::IoError,
            CombError::ParseError(_) => ErrorKind::ParseError,
            CombError::ServerError(_) => ErrorKind::ServerError,
            CombError::Timeout => ErrorKind::Timeout,
        };
        FfiError::new(kind, e.to_string())
    }
}

fn ok_json(data: impl Serialize) -> String {
    json!({ "ok": true, "data": data }).to_string()
}

fn err_json(err: &FfiError) -> String {
    json!({
        "ok": false,
        "error": { "kind": err.kind.slug(), "message": err.message },
    })
    .to_string()
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
        Err(payload) => err_json(&FfiError::new(ErrorKind::Panic, panic_message(&*payload))),
    };
    // `body` is our own serialized JSON; it cannot contain an interior NUL.
    CString::new(body)
        .expect("serialized JSON envelope must not contain a NUL byte")
        .into_raw()
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
