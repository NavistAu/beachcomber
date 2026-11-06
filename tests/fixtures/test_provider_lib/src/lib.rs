//! Beachcomber test fixture cdylib: a plugin library for `plugin(name, symbol,
//! args...)`, loaded by `tests/suite/plugin_real_ffi.rs` and
//! `tests/suite/config_layers.rs`.
//!
//! Every symbol is `extern "C" fn(*const c_char) -> *const c_char`: the input
//! is a NUL-terminated JSON array, the output a NUL-terminated JSON value that
//! the library owns (here, leaked) and `NULL` for failure.
//!
//! # Symbol inventory
//!
//! - `bc_echo_args(json)  -> *const c_char`  — returns its input array unchanged
//! - `bc_sum(json)        -> *const c_char`  — returns the sum of a numeric array as JSON
//!
//! ## Failure injection
//! - `bc_plugin_returns_null(json)         -> *const c_char`  — always NULL
//! - `bc_plugin_returns_malformed(json)    -> *const c_char`  — invalid JSON
//! - `bc_plugin_returns_invalid_utf8(json) -> *const c_char`  — bytes that are not UTF-8
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

/// Return a `*const c_char` pointing into a `'static` byte string literal.
///
/// The bytes are embedded in the binary's read-only data segment. The macro
/// appends a null terminator at compile time so `CStr::from_ptr` is safe.
macro_rules! static_cstr {
    ($s:expr) => {{
        // Concatenate a null byte to ensure null-termination.
        const BYTES: &[u8] = concat!($s, "\0").as_bytes();
        BYTES.as_ptr() as *const c_char
    }};
}

/// Hand a `String` to the caller as a leaked NUL-terminated C string.
fn leak_cstr(s: String) -> *const c_char {
    let c = CString::new(s).unwrap();
    let ptr = c.as_ptr();
    std::mem::forget(c);
    ptr
}

/// Returns its input (a JSON array) unchanged.
///
/// # Safety
///
/// `input` must point to a valid NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_echo_args(input: *const c_char) -> *const c_char {
    // SAFETY: caller guarantees a valid null-terminated C string.
    let text = unsafe { CStr::from_ptr(input) }
        .to_string_lossy()
        .into_owned();
    leak_cstr(text)
}

/// Returns the sum of the numeric JSON array in `input` as a JSON number.
///
/// # Safety
///
/// `input` must point to a valid NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_sum(input: *const c_char) -> *const c_char {
    // SAFETY: caller guarantees a valid null-terminated C string.
    let text = unsafe { CStr::from_ptr(input) }.to_string_lossy();
    let sum: f64 = text
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .filter_map(|n| n.trim().parse::<f64>().ok())
        .sum();
    leak_cstr(format!("{sum}"))
}

/// Plugin failure: always returns null.
#[unsafe(no_mangle)]
pub extern "C" fn bc_plugin_returns_null(_input: *const c_char) -> *const c_char {
    std::ptr::null()
}

/// Plugin failure: returns text that is not JSON.
#[unsafe(no_mangle)]
pub extern "C" fn bc_plugin_returns_malformed(_input: *const c_char) -> *const c_char {
    static_cstr!("{bad json}")
}

/// Plugin failure: returns a C string that is NOT valid UTF-8.
///
/// The bytes `\xc3\x28` are a two-byte start byte followed by a byte outside
/// the continuation range.
#[unsafe(no_mangle)]
pub extern "C" fn bc_plugin_returns_invalid_utf8(_input: *const c_char) -> *const c_char {
    // The null terminator is at index 2, so `CStr::from_ptr` reads these two
    // bytes and stops.
    static BYTES: [u8; 3] = [0xc3, 0x28, 0x00];
    BYTES.as_ptr() as *const c_char
}
