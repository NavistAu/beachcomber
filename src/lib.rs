pub mod boundaries;
pub mod cache;
pub mod cli;
pub mod client;
pub mod config;
pub mod daemon;
pub mod pid_check;
pub mod proc_snapshot;
pub mod protocol;
pub mod provider;
pub mod scheduler;
pub mod server;
pub mod singleton;
pub mod watcher;
pub mod watcher_registry;

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
