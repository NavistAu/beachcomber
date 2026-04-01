use beachcomber::provider::network::NetworkProvider;
use beachcomber::provider::{InvalidationStrategy, Provider};

#[test]
fn network_provider_metadata() {
    let p = NetworkProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "network");
    assert!(meta.global);
    let fields: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"interface"));
    assert!(fields.contains(&"ip"));
    assert!(fields.contains(&"vpn_active"));
    assert!(fields.contains(&"ssid"));
    assert!(fields.contains(&"online"));
    match meta.invalidation {
        InvalidationStrategy::Poll {
            interval_secs,
            floor_secs,
        } => {
            assert_eq!(interval_secs, 10);
            assert_eq!(floor_secs, 5);
        }
        _ => panic!("Expected Poll invalidation"),
    }
}

#[test]
fn network_provider_executes() {
    let p = NetworkProvider;
    let result = p.execute(None).expect("Network should always return data");
    // online should be a bool
    let online = result.get("online").unwrap().as_text();
    assert!(online == "true" || online == "false");
}
