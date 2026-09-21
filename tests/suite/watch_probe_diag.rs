//! Diagnostic (ignored by default): measure fresh-stream FSEvents delivery
//! latency at the locations that matter for the watch self-test. Run with:
//!     cargo nextest run -E 'test(fs_event_delivery_by_location)' --run-ignored all --no-capture
//!
//! Interpreting output: notify requests FSEventStream latency 0.0 + NoDefer,
//! so steady-state delivery should be milliseconds. A fresh stream also pays
//! stream-startup cost (documented in benches/prompt_race.rs as reaching
//! seconds). `TIMEOUT` means no delivery at all at that location —
//! benches/prompt_race.rs records that FSEvents does not report for
//! `$TMPDIR` (`/var/folders/...`).

use std::time::Duration;

#[tokio::test]
#[ignore]
async fn fs_event_delivery_by_location() {
    let home = std::env::var("HOME").expect("HOME set");
    let darwin_tmp = std::process::Command::new("getconf")
        .arg("DARWIN_USER_TEMP_DIR")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    let mut locations: Vec<(String, std::path::PathBuf)> = vec![
        ("env::temp_dir()".into(), std::env::temp_dir()),
        (
            "$HOME/.cache".into(),
            std::path::Path::new(&home).join(".cache"),
        ),
        ("/tmp".into(), "/tmp".into()),
    ];
    if let Some(d) = darwin_tmp {
        locations.push(("DARWIN_USER_TEMP_DIR".into(), d.into()));
    }

    for (label, base) in &locations {
        for i in 1..=3 {
            let result =
                beachcomber::watcher::self_test_native_backend_at(base, Duration::from_secs(3))
                    .await;
            match result {
                Some(d) => eprintln!("{label:>22} run {i}: delivered in {d:?}"),
                None => eprintln!("{label:>22} run {i}: TIMEOUT (3s)"),
            }
        }
    }
}
