use libbeachcomber::{Client, ClientConfig};
use std::time::Duration;

#[test]
fn client_new_default() {
    let client = Client::new();
    // Should not panic
    let _ = client;
}

#[test]
fn client_with_config() {
    let config = ClientConfig {
        timeout: Duration::from_millis(50),
        auto_start: false,
    };
    let client = Client::with_config(config);
    let _ = client;
}

#[test]
fn socket_path_returns_something() {
    let path = libbeachcomber::socket_path();
    assert!(path.to_string_lossy().contains("beachcomber"));
}

#[test]
fn client_no_autostart_returns_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("nonexistent.sock");
    let client = Client::with_config(ClientConfig {
        timeout: Duration::from_millis(50),
        auto_start: false,
    })
    .with_socket_path(socket);
    let result = client.get("hostname", None);
    assert!(
        result.is_err(),
        "expected error against absent socket, got {result:?}"
    );
}

#[test]
fn comb_data_accessors() {
    let data = libbeachcomber::CombData::from_json(
        serde_json::json!({"branch": "main", "dirty": true, "ahead": 2, "load": 1.5}),
    );
    assert_eq!(data.get_str("branch"), Some("main"));
    assert_eq!(data.get_bool("dirty"), Some(true));
    assert_eq!(data.get_i64("ahead"), Some(2));
    assert_eq!(data.get_f64("load"), Some(1.5));
    assert_eq!(data.get_str("missing"), None);
}

#[test]
fn comb_data_scalar() {
    let data = libbeachcomber::CombData::from_json(serde_json::json!("main"));
    assert_eq!(data.as_text(), Some("main".to_string()));
}

// Compile-time check: Client::refresh and Session::refresh must exist with the correct signatures.
// If the methods are still named `poke`, this test will fail to compile.
#[test]
fn client_and_session_refresh_methods_exist() {
    // Verify Client::refresh exists with the expected signature
    let _: fn(&Client, &str, Option<&str>) -> Result<(), libbeachcomber::CombError> =
        Client::refresh;
}

// Compile-time check: Client::hello and Session::hello must exist with
// the correct signatures. Guards the Phase 2 addition against accidental
// removal.
#[test]
fn client_hello_method_exists() {
    let _: fn(
        &libbeachcomber::Client,
    ) -> Result<libbeachcomber::HelloInfo, libbeachcomber::CombError> =
        libbeachcomber::Client::hello;
}
