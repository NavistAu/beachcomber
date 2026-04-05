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

#[cfg(target_os = "linux")]
mod linux_parse_tests {
    use beachcomber::provider::network::linux;

    #[test]
    fn parse_nmcli_active_network() {
        let output = "yes:HomeWifi\nno:NeighborWifi\n";
        assert_eq!(linux::parse_nmcli_output(output), Some("HomeWifi".to_string()));
    }

    #[test]
    fn parse_nmcli_no_active() {
        let output = "no:SomeNetwork\nno:OtherNetwork\n";
        assert_eq!(linux::parse_nmcli_output(output), None);
    }

    #[test]
    fn parse_nmcli_empty() {
        assert_eq!(linux::parse_nmcli_output(""), None);
    }

    #[test]
    fn parse_default_route() {
        let output = "default via 192.168.1.1 dev enp0s3 proto dhcp metric 100\n";
        assert_eq!(linux::parse_default_route(output), Some("enp0s3".to_string()));
    }

    #[test]
    fn parse_default_route_no_dev() {
        assert_eq!(linux::parse_default_route(""), None);
    }

    #[test]
    fn parse_iw_dev_interface() {
        let output = "phy#0\n\tInterface wlan0\n\t\ttype managed\n";
        assert_eq!(linux::parse_iw_dev_interface(output), Some("wlan0".to_string()));
    }

    #[test]
    fn parse_iw_info_ssid() {
        let output = "\tssid MyNetwork\n\ttype managed\n";
        assert_eq!(linux::parse_iw_info_ssid(output), Some("MyNetwork".to_string()));
    }

    #[test]
    fn vpn_detection() {
        assert!(linux::is_vpn_interface("tun0"));
        assert!(linux::is_vpn_interface("tap0"));
        assert!(linux::is_vpn_interface("wg0"));
        assert!(!linux::is_vpn_interface("eth0"));
        assert!(!linux::is_vpn_interface("enp0s3"));
    }

    #[test]
    fn preferred_interface_prefix_matching() {
        assert!(linux::is_preferred_interface_by_prefix("eth0"));
        assert!(linux::is_preferred_interface_by_prefix("ens3"));
        assert!(linux::is_preferred_interface_by_prefix("enp0s3"));
        assert!(!linux::is_preferred_interface_by_prefix("wlan0"));
    }
}
