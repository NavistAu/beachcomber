use libbeachcomber::{Client, ClientConfig};
use std::time::Duration;

/// A client that may not start a daemon, finding none on the socket it
/// resolves, says so at once: it neither spawns nor waits.
#[test]
fn client_no_autostart_returns_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("nonexistent.sock");
    let client = Client::with_config(ClientConfig {
        timeout: Duration::from_millis(50),
        auto_start: false,
        exec: false,
    });
    let result = temp_env::with_var("BEACHCOMBER_SOCKET", Some(&socket), || {
        client.get("hostname", None)
    });
    assert!(
        matches!(
            result,
            Err(libbeachcomber::resolve::ResolveError::Fetch(
                libbeachcomber::resolve::FetchError::Transport(
                    libbeachcomber::CombError::DaemonNotRunning
                )
            ))
        ),
        "expected DaemonNotRunning against an absent socket, got {result:?}"
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
