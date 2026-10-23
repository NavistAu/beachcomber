//! The `test_provider_lib` cdylib: a real plugin library to `dlopen`.

use std::path::PathBuf;
use std::sync::OnceLock;

/// The library file name on this platform, and its extension.
pub const LIB_NAME: &str = if cfg!(target_os = "macos") {
    "libtest_provider_lib.dylib"
} else {
    "libtest_provider_lib.so"
};

/// The extension a plugin library carries here (`plugin('demo', ..)` searches
/// for `libdemo.<ext>`).
pub const LIB_EXT: &str = if cfg!(target_os = "macos") {
    "dylib"
} else {
    "so"
};

fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"))
}

/// Holds an exclusive `flock` on a file until dropped.
struct FileLock(#[allow(dead_code)] std::fs::File);

impl FileLock {
    fn take(path: &std::path::Path) -> Self {
        use std::os::fd::AsRawFd;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .unwrap_or_else(|e| panic!("open lock {path:?}: {e}"));
        // SAFETY: the descriptor is open for the duration of the call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        assert_eq!(rc, 0, "flock {path:?}: {}", std::io::Error::last_os_error());
        FileLock(file)
    }
}

/// The built fixture, always (re)built first with the cargo that built this
/// test and the profile this test was built with, so a stale library from an
/// earlier build never answers. Once per process. Absolute.
///
/// Every test process builds, and even a build with nothing to do replaces
/// `target/<profile>/libtest_provider_lib.*` with a new file, so a path into
/// that directory can vanish under a concurrent process. The build therefore
/// runs under a lock shared by every process, and what it returns is a copy
/// named for its content: written once, never replaced, safe to open, link and
/// load whatever else is building.
pub fn ensure_fixture_built() -> PathBuf {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let release = !cfg!(debug_assertions);
            let profile = if release { "release" } else { "debug" };
            let out_dir = target_dir().join(profile);
            std::fs::create_dir_all(&out_dir).expect("create target profile dir");
            let _lock = FileLock::take(&out_dir.join(".test_provider_lib.lock"));

            let mut build = std::process::Command::new(env!("CARGO"));
            build
                .args(["build", "-p", "test_provider_lib"])
                .current_dir(env!("CARGO_MANIFEST_DIR"));
            if release {
                build.arg("--release");
            }
            let out = build.output().expect("cargo build invocation failed");
            assert!(
                out.status.success(),
                "fixture build failed:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let built = out_dir.join(LIB_NAME);
            let bytes = std::fs::read(&built)
                .unwrap_or_else(|e| panic!("fixture artifact missing at {built:?}: {e}"));
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&bytes, &mut hasher);
            let name = format!(
                "test_provider_lib-{:016x}",
                std::hash::Hasher::finish(&hasher)
            );
            let dir = out_dir.join("test-fixtures").join(name);
            std::fs::create_dir_all(&dir).expect("create fixture dir");
            let copy = dir.join(LIB_NAME);
            if !copy.exists() {
                let partial = dir.join(format!("{LIB_NAME}.partial"));
                std::fs::write(&partial, &bytes).expect("write fixture copy");
                std::fs::rename(&partial, &copy).expect("publish fixture copy");
            }
            copy.canonicalize()
                .unwrap_or_else(|e| panic!("fixture copy missing at {copy:?}: {e}"))
        })
        .clone()
}
