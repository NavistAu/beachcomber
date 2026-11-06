//! Dynamic library loader boundary trait.
//!
//! The seam between `plugin()` (`crate::engine::PluginSet`) and the real
//! `libloading` crate. Every call into a plugin symbol goes through
//! [`call_symbol`], whether `dlsym` found it (`LibloadingLoadedLibrary`) or the
//! compiled-in table did (`crate::plugin::StaticLibrary`); the engine only
//! calls the safe `LoadedLibrary` trait method.

#[cfg_attr(test, mockall::automock)]
pub trait LibraryLoader: Send + Sync {
    fn load(&self, path: String) -> Result<Box<dyn LoadedLibrary>, String>;
}

/// A successfully loaded plugin library.
#[cfg_attr(test, mockall::automock)]
pub trait LoadedLibrary: Send + Sync {
    /// Call a plugin symbol `fn(*const c_char) -> *const c_char`: `input` is a
    /// NUL-terminated JSON array, the result a JSON value owned by the library.
    /// Each failure (symbol not found, input holding a NUL byte, NULL return,
    /// non-UTF-8 output) is its own `Err` message.
    fn call_json(&self, symbol: String, input: String) -> Result<String, String>;
    /// Which loaded image this is: the `dlopen` handle (every load uses the same
    /// flags, as macOS encodes them in the handle), which the system
    /// returns again for a file it already holds, whatever name reached it.
    fn identity(&self) -> usize;
}

// ── Real implementation ───────────────────────────────────────────────────────

pub struct LibloadingLoader;

impl LibraryLoader for LibloadingLoader {
    fn load(&self, path: String) -> Result<Box<dyn LoadedLibrary>, String> {
        // SAFETY: We trust the caller to supply a valid library path.
        let lib = unsafe { libloading::os::unix::Library::new(&path) }
            .map_err(|e| format!("failed to load library '{}': {}", path, e))?;
        // The handle names the image: `dlopen` returns the same one for a file
        // it already holds. `into_raw`/`from_raw` read it without closing.
        let handle = lib.into_raw();
        let image = handle as usize;
        // SAFETY: `handle` came from `into_raw` just above and is used once.
        let lib = unsafe { libloading::os::unix::Library::from_raw(handle) };
        Ok(Box::new(LibloadingLoadedLibrary {
            lib: std::sync::Mutex::new(libloading::Library::from(lib)),
            image,
        }))
    }
}

/// Holds a loaded `libloading::Library` behind a `Mutex`, which serialises
/// calls into it: a plugin is never entered by two threads at once.
pub struct LibloadingLoadedLibrary {
    lib: std::sync::Mutex<libloading::Library>,
    image: usize,
}

impl LoadedLibrary for LibloadingLoadedLibrary {
    fn identity(&self) -> usize {
        self.image
    }

    fn call_json(&self, symbol: String, input: String) -> Result<String, String> {
        let c_symbol = std::ffi::CString::new(symbol.as_str())
            .map_err(|_| "the symbol name contains a NUL byte".to_string())?;
        let lib = self
            .lib
            .lock()
            .map_err(|_| "the library lock is poisoned".to_string())?;
        // The lock is held across the call and the copy out of the returned
        // string: that is what keeps "valid until the next call into the same
        // library" true, since no other call can start before we have copied.
        // SAFETY: symbol lookup is unsafe; we trust the plugin contract.
        let f: libloading::Symbol<JsonFn> = unsafe { lib.get(c_symbol.to_bytes_with_nul()) }
            .map_err(|_| "symbol not found".to_string())?;
        call_symbol(*f, input)
    }
}

/// A plugin symbol: `const char* symbol(const char* input)`, the plugin
/// author's contract ([`crate::engine::PluginSet`]).
pub type JsonFn = unsafe extern "C" fn(*const std::os::raw::c_char) -> *const std::os::raw::c_char;

/// Call the plugin symbol `f` with `input`, a JSON array: the one way every
/// plugin call is made, a library's (found by `dlsym`) and a compiled-in
/// one's ([`crate::plugin::StaticLibrary`]) alike. The input goes in as a
/// NUL-terminated string; `NULL` back is failure; what comes back otherwise is
/// copied out as strict UTF-8 before this returns, so the caller decides how
/// long the symbol's own string must live. A panic cannot unwind out of an
/// `extern "C"` function (it aborts the process), so there is nothing to
/// catch.
pub fn call_symbol(f: JsonFn, input: String) -> Result<String, String> {
    let c_input =
        std::ffi::CString::new(input).map_err(|_| "the input contains a NUL byte".to_string())?;
    // SAFETY: `f` is a plugin symbol, which takes a NUL-terminated string and
    // returns NULL or one: the contract we trust it to keep.
    let ptr = unsafe { f(c_input.as_ptr()) };
    if ptr.is_null() {
        return Err("returned NULL".to_string());
    }
    // Strict UTF-8, as everywhere in this boundary.
    // SAFETY: a non-NULL return is a NUL-terminated string, valid until the
    // next call into the same library, which cannot start before this copy.
    unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_str()
        .map(str::to_string)
        .map_err(|_| "returned output that is not UTF-8".to_string())
}
