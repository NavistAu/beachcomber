pub mod boundaries;
pub mod cache;
pub mod cli;
pub mod client;
pub mod config;
pub mod daemon;
pub mod engine;
pub mod pid_check;
pub mod proc_snapshot;
pub mod protocol;
pub mod provider;
pub mod query;
pub mod scheduler;
pub mod server;
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
#[ctor::ctor]
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
