//! Lint: integration tests must not rely on default socket discovery, which
//! makes them fail when a real daemon is present at $XDG_RUNTIME_DIR/beachcomber/sock.
//! Use `ClientConfig::with_socket_path(tempdir().join("foo.sock"))` instead.

use std::fs;
use std::path::Path;

#[test]
fn no_test_uses_default_socket_discovery() {
    let denied_patterns = [
        "ClientConfig::default()",
        "libbeachcomber::socket_path()",
    ];
    let allow_list = [
        "tests/test_isolation.rs",
        "tests/conformance/socket_default_discovery.rs",
        // socket_path_returns_something is a compile/return-value check, no I/O.
        "beachcomber-client/tests/client_test.rs",
    ];

    let tests_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let client_tests_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("beachcomber-client/tests");

    let mut offenders = Vec::new();
    for entry in walkdir::WalkDir::new(&tests_root)
        .into_iter()
        .chain(walkdir::WalkDir::new(&client_tests_root))
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let path_str = path.to_string_lossy();
        if allow_list.iter().any(|a| path_str.ends_with(a)) {
            continue;
        }
        let body = fs::read_to_string(path).unwrap_or_default();
        for pat in denied_patterns {
            if body.contains(pat) {
                offenders.push(format!("{}: {}", path_str, pat));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "tests using default socket discovery (must be allow-listed or fixed):\n{}",
        offenders.join("\n")
    );
}
