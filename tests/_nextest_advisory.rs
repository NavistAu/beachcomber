//! Advisory test. Fails under `cargo test` so contributors are prompted
//! to use `cargo nextest run` (or the `cargo t` alias) instead. Nextest
//! sets the `NEXTEST` environment variable when it spawns test binaries,
//! so this test passes under nextest but panics under plain cargo test.

#[test]
fn prefer_nextest_runner() {
    if std::env::var("NEXTEST").is_err() {
        panic!(
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
    }
}
