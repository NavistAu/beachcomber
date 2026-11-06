//! A test's client handle and the envelope it reads back: one copy for every
//! module, so a choice such as how long to keep connecting is made once.

use std::ffi::{CStr, CString, c_char};
use std::path::Path;

use beachcomber::envelope::bc_string_free;
use beachcomber::{BcClient, bc_client_new};

/// What a test client sets beyond the socket.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// `exec`: templates it renders may call `exec()`.
    pub exec: bool,
    /// `connect_timeout_ms`; `None` keeps the ABI's default. A test of a
    /// socket nobody serves passes `Some(0)`, one attempt, rather than wait
    /// out the default's retries.
    pub connect_timeout_ms: Option<u64>,
}

/// `bc_client_new` for the daemon at `sock`: no autostart, two seconds for
/// a read or a write, otherwise the ABI's defaults.
#[allow(dead_code)]
pub fn client_for(sock: &Path) -> *mut BcClient {
    client_with(sock, Options::default())
}

/// [`client_for`] with `options`.
pub fn client_with(sock: &Path, options: Options) -> *mut BcClient {
    let mut json = serde_json::json!({
        "socket_path": sock,
        "autostart": false,
        "timeout_ms": 2000,
    });
    if options.exec {
        json["exec"] = true.into();
    }
    if let Some(ms) = options.connect_timeout_ms {
        json["connect_timeout_ms"] = ms.into();
    }
    let json = cs(&json.to_string());
    unsafe { bc_client_new(json.as_ptr()) }
}

/// A C string of `s`.
pub fn cs(s: &str) -> CString {
    CString::new(s).unwrap()
}

/// The envelope at `ptr`, freed.
#[allow(dead_code)]
pub fn read(ptr: *mut c_char) -> serde_json::Value {
    assert!(!ptr.is_null(), "an FFI call must never return NULL");
    let body = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_string();
    unsafe { bc_string_free(ptr) };
    serde_json::from_str(&body).expect("envelope must be valid JSON")
}
