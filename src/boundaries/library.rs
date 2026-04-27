//! Dynamic library loader boundary trait.

#[cfg_attr(test, mockall::automock)]
pub trait LibraryLoader: Send + Sync {
    fn load(&self, path: String) -> Result<Box<dyn LoadedLibrary>, String>;
}

#[cfg_attr(test, mockall::automock)]
pub trait LoadedLibrary: Send + Sync {
    fn call_metadata(&self, symbol: String) -> Option<String>;
    fn call_source_metadata(&self, idx: usize) -> Option<String>;
    fn call_execute(&self, symbol: String, path: Option<String>) -> Option<String>;
    fn call_source_execute(&self, idx: usize, path: Option<String>) -> Option<String>;
    fn source_count(&self) -> usize;
}

pub struct LibloadingLoader;

impl LibraryLoader for LibloadingLoader {
    fn load(&self, _path: String) -> Result<Box<dyn LoadedLibrary>, String> {
        // Real impl wraps libloading::Library::new and returns a wrapper holding the
        // Library + dispatching the C-ABI calls. Lands in P3.8.
        todo!("move libloading calls from provider/library.rs in P3.8")
    }
}
