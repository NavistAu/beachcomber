//! Dynamic library loader boundary trait.
//!
//! The seam between `provider/library.rs` and the real `libloading` crate.
//! All `unsafe` FFI calls live in `LibloadingLoadedLibrary`; the provider
//! only calls the safe `LoadedLibrary` trait methods.

#[cfg_attr(test, mockall::automock)]
pub trait LibraryLoader: Send + Sync {
    fn load(&self, path: String) -> Result<Box<dyn LoadedLibrary>, String>;
}

/// Methods provided by a successfully loaded shared library.
///
/// Return `None` on any of:
/// - symbol absent
/// - FFI function returns null
/// - FFI function panics (caught by `catch_unwind`)
/// - returned C string is invalid UTF-8
///
/// `source_count` returns 0 to signal that the library uses the legacy
/// single-source ABI (i.e. `bc_source_count` symbol is absent).
#[cfg_attr(test, mockall::automock)]
pub trait LoadedLibrary: Send + Sync {
    /// Call a zero-argument C function that returns `*const c_char` (legacy metadata).
    /// `symbol` is the C symbol name, e.g. `"beachcomber_provider_metadata"`.
    fn call_metadata(&self, symbol: String) -> Option<String>;
    /// Call `bc_source_metadata(idx)`.
    fn call_source_metadata(&self, idx: usize) -> Option<String>;
    /// Call a C function `fn(*const c_char) -> *const c_char` (legacy execute).
    /// `symbol` is the C symbol name, e.g. `"beachcomber_provider_execute"`.
    fn call_execute(&self, symbol: String, path: Option<String>) -> Option<String>;
    /// Call `bc_source_execute(idx, path)`.
    fn call_source_execute(&self, idx: usize, path: Option<String>) -> Option<String>;
    /// Return the value of `bc_source_count()`, or 0 if the symbol is absent
    /// (indicating legacy single-source ABI).
    fn source_count(&self) -> usize;
}

// ── Real implementation ───────────────────────────────────────────────────────

pub struct LibloadingLoader;

impl LibraryLoader for LibloadingLoader {
    fn load(&self, path: String) -> Result<Box<dyn LoadedLibrary>, String> {
        // SAFETY: We trust the caller to supply a valid library path.
        let lib = unsafe { libloading::Library::new(&path) }
            .map_err(|e| format!("failed to load library '{}': {}", path, e))?;
        Ok(Box::new(LibloadingLoadedLibrary {
            lib: std::sync::Mutex::new(lib),
        }))
    }
}

/// Holds a loaded `libloading::Library` behind a `Mutex` so it is `Send + Sync`.
///
/// Each `call_*` method:
/// 1. Locks the mutex.
/// 2. Looks up the symbol; returns `None` if absent.
/// 3. Calls the C function inside `std::panic::catch_unwind` so a misbehaving
///    shared library cannot abort the daemon.
/// 4. Reads the returned `*const c_char`, converts to `String`; returns `None`
///    if the pointer is null or the bytes are not valid UTF-8.
pub struct LibloadingLoadedLibrary {
    lib: std::sync::Mutex<libloading::Library>,
}

// SAFETY: The underlying library functions are required to be thread-safe per
// the beachcomber provider contract. The Mutex serialises accesses.
unsafe impl Send for LibloadingLoadedLibrary {}
unsafe impl Sync for LibloadingLoadedLibrary {}

impl LibloadingLoadedLibrary {
    /// Look up a zero-argument symbol `sym` that returns `*const c_char`, call
    /// it inside `catch_unwind`, and convert the result to a `String`.
    fn call_no_arg_str(&self, sym: &[u8]) -> Option<String> {
        type NoArgStrFn = unsafe extern "C" fn() -> *const std::os::raw::c_char;
        let lib = self.lib.lock().ok()?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // SAFETY: symbol lookup is unsafe; we trust the library contract.
            let f: libloading::Symbol<NoArgStrFn> = unsafe { lib.get(sym) }.ok()?;
            let ptr = unsafe { f() };
            if ptr.is_null() {
                return None;
            }
            let cstr = unsafe { std::ffi::CStr::from_ptr(ptr) };
            Some(cstr.to_string_lossy().into_owned())
        }));
        match result {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    "catch_unwind caught a panic in FFI symbol '{}'",
                    String::from_utf8_lossy(sym).trim_end_matches('\0')
                );
                None
            }
        }
    }

    /// Call `bc_source_metadata(idx)`.
    fn call_bc_source_metadata_inner(&self, idx: usize) -> Option<String> {
        type BcSourceMetadataFn = unsafe extern "C" fn(usize) -> *const std::os::raw::c_char;
        let lib = self.lib.lock().ok()?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let f: libloading::Symbol<BcSourceMetadataFn> =
                unsafe { lib.get(b"bc_source_metadata\0") }.ok()?;
            let ptr = unsafe { f(idx) };
            if ptr.is_null() {
                return None;
            }
            let cstr = unsafe { std::ffi::CStr::from_ptr(ptr) };
            Some(cstr.to_string_lossy().into_owned())
        }));
        match result {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!("catch_unwind caught a panic in bc_source_metadata({})", idx);
                None
            }
        }
    }

    /// Call a legacy execute symbol: `fn(*const c_char) -> *const c_char`.
    fn call_execute_inner(&self, sym: &[u8], path: Option<String>) -> Option<String> {
        type ExecuteFn =
            unsafe extern "C" fn(*const std::os::raw::c_char) -> *const std::os::raw::c_char;
        let lib = self.lib.lock().ok()?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let f: libloading::Symbol<ExecuteFn> = unsafe { lib.get(sym) }.ok()?;
            let c_path = path.as_deref().and_then(|p| std::ffi::CString::new(p).ok());
            let path_ptr = c_path.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            let ptr = unsafe { f(path_ptr) };
            if ptr.is_null() {
                return None;
            }
            let cstr = unsafe { std::ffi::CStr::from_ptr(ptr) };
            Some(cstr.to_string_lossy().into_owned())
        }));
        match result {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    "catch_unwind caught a panic in FFI execute symbol '{}'",
                    String::from_utf8_lossy(sym).trim_end_matches('\0')
                );
                None
            }
        }
    }

    /// Call `bc_source_execute(idx, path)`.
    fn call_bc_source_execute_inner(&self, idx: usize, path: Option<String>) -> Option<String> {
        type BcSourceExecuteFn =
            unsafe extern "C" fn(usize, *const std::os::raw::c_char) -> *const std::os::raw::c_char;
        let lib = self.lib.lock().ok()?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let f: libloading::Symbol<BcSourceExecuteFn> =
                unsafe { lib.get(b"bc_source_execute\0") }.ok()?;
            let c_path = path.as_deref().and_then(|p| std::ffi::CString::new(p).ok());
            let path_ptr = c_path.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            let ptr = unsafe { f(idx, path_ptr) };
            if ptr.is_null() {
                return None;
            }
            let cstr = unsafe { std::ffi::CStr::from_ptr(ptr) };
            Some(cstr.to_string_lossy().into_owned())
        }));
        match result {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!("catch_unwind caught a panic in bc_source_execute({})", idx);
                None
            }
        }
    }
}

impl LoadedLibrary for LibloadingLoadedLibrary {
    fn call_metadata(&self, symbol: String) -> Option<String> {
        // Append null terminator for the C string symbol lookup.
        let mut sym = symbol.into_bytes();
        sym.push(0);
        self.call_no_arg_str(&sym)
    }

    fn call_source_metadata(&self, idx: usize) -> Option<String> {
        self.call_bc_source_metadata_inner(idx)
    }

    fn call_execute(&self, symbol: String, path: Option<String>) -> Option<String> {
        let mut sym = symbol.into_bytes();
        sym.push(0);
        self.call_execute_inner(&sym, path)
    }

    fn call_source_execute(&self, idx: usize, path: Option<String>) -> Option<String> {
        self.call_bc_source_execute_inner(idx, path)
    }

    fn source_count(&self) -> usize {
        type BcSourceCountFn = unsafe extern "C" fn() -> usize;
        let lib = match self.lib.lock() {
            Ok(l) => l,
            Err(_) => return 0,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let f: libloading::Symbol<BcSourceCountFn> =
                unsafe { lib.get(b"bc_source_count\0") }.ok()?;
            Some(unsafe { f() })
        }));
        match result {
            Ok(Some(n)) => n,
            Ok(None) => 0, // symbol absent → legacy ABI
            Err(_) => {
                tracing::warn!("catch_unwind caught a panic in bc_source_count()");
                0
            }
        }
    }
}
