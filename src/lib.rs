pub mod boundaries;
pub mod cache;
pub mod cli;
pub mod client;
pub mod config;
pub mod daemon;
pub mod engine;
pub mod persist;
pub mod plugin;
pub mod proc_snapshot;
pub mod protocol;
pub mod provider;
pub mod query;
pub mod run;
pub mod scheduler;
pub mod server;
pub mod shape;
pub mod singleton;
pub mod watcher;
pub mod watcher_registry;

#[cfg(test)]
mod cache_staleness_tests;
#[cfg(test)]
mod config_tests;
#[cfg(test)]
mod proc_snapshot_tests;
#[cfg(test)]
mod protocol_tests;
#[cfg(test)]
mod watch_registry_tests;

#[cfg(test)]
#[ctor::ctor(unsafe)]
fn prefer_nextest_advisory() {
    if std::env::var("NEXTEST").is_err() {
        eprintln!(
            "\n\n\
            ===================================================================\n\
            beachcomber prefers the cargo-nextest runner. Plain `cargo test`\n\
            bypasses our per-test kill timeouts (.config/nextest.toml) and\n\
            makes hung tests hard to catch.\n\
            \n\
            Use one of:\n\
              cargo nextest run          # full runner, respects our config\n\
              cargo t                    # shorthand alias we ship\n\
            \n\
            Install cargo-nextest (one-time):\n\
              mise install               # if you have mise, picks up mise.toml\n\
              cargo install cargo-nextest --locked\n\
            \n\
            To bypass this advisory intentionally, set NEXTEST=1 in the env.\n\
            ===================================================================\n\n"
        );
        std::process::exit(2);
    }
}

/// A test process keeps its state and cache (`$XDG_STATE_HOME`,
/// `$XDG_CACHE_HOME`) in a directory of its own, removed when it exits: never
/// the developer's. A config load that migrates a file locks `migrate.lock`
/// and rewrites `migrated.json` in the state directory; shared, every test
/// process queued on the one lock and grew the developer's record by a
/// temp path each. A test about those directories sets its own.
#[cfg(test)]
#[ctor::ctor(unsafe)]
fn isolate_state_dirs() {
    let dir = std::env::temp_dir().join(format!("beachcomber-test-{}", std::process::id()));
    // SAFETY: a constructor runs before `main`, while the process has one
    // thread; the exit handler only removes the directory.
    unsafe {
        std::env::set_var("XDG_STATE_HOME", dir.join("state"));
        std::env::set_var("XDG_CACHE_HOME", dir.join("cache"));
        libc::atexit(remove_state_dirs);
    }
}

#[cfg(test)]
extern "C" fn remove_state_dirs() {
    let _ = std::fs::remove_dir_all(
        std::env::temp_dir().join(format!("beachcomber-test-{}", std::process::id())),
    );
}

/// The root crate and `beachcomber-client` derive `BEACHCOMBER_VERSION`
/// independently at build time (see `build-common/version.rs`), each from its
/// own `build.rs`. This asserts the two derivations never disagree.
#[cfg(test)]
mod version_identity_tests {
    #[test]
    fn root_and_client_report_the_same_version() {
        assert_eq!(env!("BEACHCOMBER_VERSION"), libbeachcomber::VERSION);
    }
}
