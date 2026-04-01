use beachcomber_client::{Client, ClientConfig};
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
    let path = beachcomber_client::socket_path();
    assert!(path.to_string_lossy().contains("beachcomber"));
}

#[test]
fn client_no_autostart_returns_error() {
    let config = ClientConfig {
        timeout: Duration::from_millis(50),
        auto_start: false,
    };
    let client = Client::with_config(config);
    // With auto_start disabled and no daemon running on a random socket,
    // this should return DaemonNotRunning or ConnectionFailed
    let result = client.get("hostname", None);
    assert!(result.is_err());
}

#[test]
fn comb_data_accessors() {
    let data = beachcomber_client::CombData::from_json(
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
    let data = beachcomber_client::CombData::from_json(serde_json::json!("main"));
    assert_eq!(data.as_text(), Some("main".to_string()));
}
