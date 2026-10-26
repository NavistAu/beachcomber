use crate::provider::network::NetworkProvider;
use crate::provider::{InvalidationStrategy, Provider};
// The scan, the SSID parsers and the interface rules are `sys`'s, tested there
// (`src/plugin/sys/`).

#[test]
fn network_provider_metadata() {
    let p = NetworkProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "network");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "interfaces");
    let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"interface"));
    assert!(fields.contains(&"ip"));
    assert!(fields.contains(&"vpn_active"));
    assert!(fields.contains(&"ssid"));
    assert!(fields.contains(&"online"));
    match src.invalidation {
        InvalidationStrategy::Poll { interval_secs, .. } => {
            assert_eq!(interval_secs, 10);
        }
        _ => panic!("Expected Poll invalidation"),
    }
}

#[test]
fn network_provider_executes() {
    let p = NetworkProvider;
    let sources = p.sources();
    let result = sources[0].execute(None).unwrap();
    // Network should always return data (online may be false but fields present)
    let online = result.fields.get("online").unwrap().as_text();
    assert!(online == "true" || online == "false");
}

// IPv6 test: check the field schema declares ipv6.
#[test]
fn network_provider_metadata_includes_ipv6_field() {
    let meta = NetworkProvider.metadata();
    let fields: Vec<&str> = meta.sources[0]
        .fields
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert!(
        fields.contains(&"ipv6"),
        "network provider must declare an ipv6 field; got: {:?}",
        fields
    );
}

#[cfg(target_os = "linux")]
mod linux_parse_tests {
    use crate::provider::network::linux;

    #[test]
    fn parse_default_route() {
        let output = "default via 192.168.1.1 dev enp0s3 proto dhcp metric 100\n";
        assert_eq!(
            linux::parse_default_route(output),
            Some("enp0s3".to_string())
        );
    }

    #[test]
    fn parse_default_route_no_dev() {
        assert_eq!(linux::parse_default_route(""), None);
    }
}
