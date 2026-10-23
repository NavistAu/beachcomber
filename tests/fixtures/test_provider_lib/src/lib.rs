/// Beachcomber test fixture cdylib.
///
/// Exports both the Phase 4 multi-source ABI and the legacy single-source ABI
/// so that `tests/suite/provider_library_real_ffi.rs` can exercise every code path
/// in `src/boundaries/library.rs` via `LibloadingLoader`.
///
/// # Symbol inventory
///
/// ## Phase 4 multi-source ABI
/// - `bc_source_count()         -> usize`            — returns 2
/// - `bc_source_metadata(idx)   -> *const c_char`    — JSON for idx 0 or 1; null for idx >= 2
/// - `bc_source_execute(idx, p) -> *const c_char`    — JSON for idx 0 or 1; null for idx >= 2
///
/// ## Legacy single-source ABI
/// - `beachcomber_provider_metadata()       -> *const c_char`  — valid JSON
/// - `beachcomber_provider_execute(path)    -> *const c_char`  — valid JSON
///
/// ## Plugin ABI (`plugin(name, symbol, args...)`)
/// - `bc_echo_args(json)  -> *const c_char`  — returns its input array unchanged
/// - `bc_sum(json)        -> *const c_char`  — returns the sum of a numeric array as JSON
/// - `bc_plugin_returns_null(json)      -> *const c_char`  — always NULL
/// - `bc_plugin_returns_malformed(json) -> *const c_char`  — invalid JSON
///
/// ## Error-injection helpers
/// - `bc_metadata_returns_null()  -> *const c_char`  — always returns null
/// - `bc_execute_returns_null(p)  -> *const c_char`  — always returns null
/// - `bc_metadata_malformed()     -> *const c_char`  — returns invalid JSON
/// - `bc_execute_malformed(p)     -> *const c_char`  — returns invalid JSON
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

// ── helpers ───────────────────────────────────────────────────────────────────

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

// ── Phase 4 multi-source ABI ──────────────────────────────────────────────────

/// Return the number of sources exported by this library.
#[unsafe(no_mangle)]
pub extern "C" fn bc_source_count() -> usize {
    2
}

/// Return metadata JSON for source `idx`.  Returns null for `idx >= 2`.
///
/// Source 0 — global, poll-based.
/// Source 1 — path-scoped, poll-based.
#[unsafe(no_mangle)]
pub extern "C" fn bc_source_metadata(idx: usize) -> *const c_char {
    match idx {
        0 => static_cstr!(
            r#"{"name":"alpha","fields":{"value":"string","count":"int"},"global":true}"#
        ),
        1 => static_cstr!(r#"{"name":"beta","fields":{"flag":"bool"},"global":false}"#),
        _ => std::ptr::null(),
    }
}

/// Execute source `idx` with an optional path.  Returns null for `idx >= 2`.
///
/// Source 0 ignores `path` and returns a fixed result.
/// Source 1 echoes the path (or "none") in its output.
///
/// # Safety
///
/// `path` must be null or point to a valid NUL-terminated C string that
/// stays alive for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bc_source_execute(idx: usize, path: *const c_char) -> *const c_char {
    match idx {
        0 => static_cstr!(r#"{"value":"hello","count":42}"#),
        1 => {
            // Echo the path so the test can verify the argument is threaded correctly.
            let path_str = if path.is_null() {
                "none".to_string()
            } else {
                // SAFETY: caller guarantees a valid null-terminated C string.
                unsafe { CStr::from_ptr(path) }
                    .to_string_lossy()
                    .into_owned()
            };
            let json = format!(r#"{{"flag":true,"path":"{}"}}"#, path_str);
            let c = CString::new(json).unwrap();
            let ptr = c.as_ptr();
            std::mem::forget(c);
            ptr
        }
        _ => std::ptr::null(),
    }
}

// ── Legacy single-source ABI ──────────────────────────────────────────────────

/// Legacy metadata symbol queried via `call_metadata("beachcomber_provider_metadata")`.
#[unsafe(no_mangle)]
pub extern "C" fn beachcomber_provider_metadata() -> *const c_char {
    static_cstr!(r#"{"name":"legacy","fields":{"info":"string"},"global":true}"#)
}

/// Legacy execute symbol queried via `call_execute("beachcomber_provider_execute", path)`.
#[unsafe(no_mangle)]
pub extern "C" fn beachcomber_provider_execute(_path: *const c_char) -> *const c_char {
    static_cstr!(r#"{"info":"legacy_result"}"#)
}

// ── Error-injection helpers ───────────────────────────────────────────────────

/// Always returns null — used to verify that `call_metadata` returns `None`.
#[unsafe(no_mangle)]
pub extern "C" fn bc_metadata_returns_null() -> *const c_char {
    std::ptr::null()
}

/// Always returns null — used to verify that `call_execute` returns `None`.
#[unsafe(no_mangle)]
pub extern "C" fn bc_execute_returns_null(_path: *const c_char) -> *const c_char {
    std::ptr::null()
}

/// Returns invalid JSON — used to verify graceful failure in `call_metadata`.
#[unsafe(no_mangle)]
pub extern "C" fn bc_metadata_malformed() -> *const c_char {
    static_cstr!("{bad json}")
}

/// Returns invalid JSON — used to verify graceful failure in `call_execute`.
#[unsafe(no_mangle)]
pub extern "C" fn bc_execute_malformed(_path: *const c_char) -> *const c_char {
    static_cstr!("{bad json}")
}

/// Returns a C string that is NOT valid UTF-8.
///
/// The bytes `\xc3\x28` form an overlong / invalid UTF-8 sequence (a two-byte
/// start byte followed by a byte outside the expected continuation range).
/// Used by `tests/suite/provider_library_real_ffi.rs` to assert that the strict-UTF-8
/// boundary returns `None` rather than substituting U+FFFD.
#[unsafe(no_mangle)]
pub extern "C" fn bc_metadata_invalid_utf8() -> *const c_char {
    // SAFETY: This is intentionally invalid UTF-8. The null terminator is at
    // index 2 so CStr::from_ptr will read exactly these two bytes then stop.
    static BYTES: [u8; 3] = [0xc3, 0x28, 0x00];
    BYTES.as_ptr() as *const c_char
}

// ── Plugin ABI ────────────────────────────────────────────────────────────────

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
