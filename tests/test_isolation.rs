//! Lint: integration tests must not rely on default socket discovery, which
//! makes them fail when a real daemon is present at $XDG_RUNTIME_DIR/beachcomber/sock.
//! Use `ClientConfig::with_socket_path(tempdir().join("foo.sock"))` instead.

use std::fs;
use std::path::Path;

#[test]
fn no_test_uses_default_socket_discovery() {
    let denied_patterns = ["ClientConfig::default()", "libbeachcomber::socket_path()"];
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

/// Extract function bodies from a Rust source file.
///
/// Returns a `Vec` of `(fn_name, body_text)` pairs where `body_text` is
/// the source text between (and including) the opening and closing braces of
/// the function, built by walking character-by-character with a brace-depth
/// state machine.
///
/// This is intentionally simple: it handles the 95% case of ordinary test
/// functions.  Macros that contain `fn` keywords or functions defined inside
/// string literals could theoretically trip it, but those do not appear in
/// this test suite.
fn extract_function_bodies(source: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    // Collect (byte_offset, char) pairs so we can do safe byte-offset slicing.
    let indexed: Vec<(usize, char)> = source.char_indices().collect();
    let len = indexed.len();
    let mut i = 0;

    while i < len {
        // Look for the ASCII sequence "fn " (3 chars, all single-byte).
        let looks_like_fn = i + 2 < len
            && indexed[i].1 == 'f'
            && indexed[i + 1].1 == 'n'
            && indexed[i + 2].1 == ' ';

        if looks_like_fn {
            // Capture the function name: the identifier immediately after "fn ".
            let name_start_idx = i + 3;
            let name_end_idx = indexed[name_start_idx..]
                .iter()
                .position(|(_, c)| !c.is_alphanumeric() && *c != '_')
                .map(|p| name_start_idx + p)
                .unwrap_or(len);

            let fn_name = if name_start_idx < len {
                let byte_start = indexed[name_start_idx].0;
                let byte_end = if name_end_idx < len {
                    indexed[name_end_idx].0
                } else {
                    source.len()
                };
                source[byte_start..byte_end].to_string()
            } else {
                String::new()
            };

            // Scan forward to find the opening `{` of this function body,
            // skipping the parameter list and return type.
            let mut j = i;
            while j < len && indexed[j].1 != '{' {
                j += 1;
            }
            if j >= len {
                break;
            }

            // Walk the brace-balanced body using character iteration.
            let body_start_byte = indexed[j].0;
            let mut depth: usize = 0;
            let mut k = j;
            let mut finished = false;
            while k < len {
                match indexed[k].1 {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            // body_end_byte is the byte just after the closing `}`.
                            let body_end_byte = if k + 1 < len {
                                indexed[k + 1].0
                            } else {
                                source.len()
                            };
                            results.push((
                                fn_name.clone(),
                                source[body_start_byte..body_end_byte].to_string(),
                            ));
                            i = k + 1;
                            finished = true;
                            break;
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
            if !finished {
                // Unmatched brace — skip past this "fn ".
                i += 3;
            }
        } else {
            i += 1;
        }
    }

    results
}

#[test]
fn no_with_config_without_socket_path() {
    // Any file in this list is entirely skipped for the with_config check.
    //
    // `beachcomber-client/tests/client_test.rs` contains a deliberate
    // no-I/O compile check (`client_with_config`) that constructs a client
    // without a socket path because it never calls any network method.
    let allow_list = [
        "tests/test_isolation.rs",
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

        let source = fs::read_to_string(path).unwrap_or_default();

        // Fast-path: skip files that never mention with_config at all.
        if !source.contains("Client::with_config(") {
            continue;
        }

        for (fn_name, body) in extract_function_bodies(&source) {
            if body.contains("Client::with_config(") && !body.contains(".with_socket_path(") {
                offenders.push(format!(
                    "{}::{} — Client::with_config used without .with_socket_path",
                    path_str, fn_name
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "tests use Client::with_config without chaining .with_socket_path \
         (add .with_socket_path(tmp.join(\"foo.sock\")) or add to allow_list):\n{}",
        offenders.join("\n")
    );
}
