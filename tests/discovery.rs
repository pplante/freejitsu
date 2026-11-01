mod support;

use std::net::Ipv4Addr;

use freejitsu::discovery::parse_reply;

#[test]
fn parses_a_valid_reply() {
    let config = support::DiscoveryConfig {
        scan_port: 40001,
        control_port: 40002,
        paired_host_id: [1, 2, 3, 4, 5, 6, 7, 8],
        ..support::DiscoveryConfig::default()
    };
    let data = support::discovery_reply(&config);
    let info = parse_reply(&data, Ipv4Addr::LOCALHOST).expect("valid reply");
    assert_eq!(info.ip, Ipv4Addr::LOCALHOST);
    assert_eq!(info.scan_port, 40001);
    assert_eq!(info.control_port, 40002);
    assert_eq!(info.mac, config.mac);
    assert_eq!(info.name, "iX500-A0XX012345");
    assert_eq!(info.model, "ScanSnap iX500");
    assert_eq!(info.if_version, 6);
    assert_eq!(info.serial, "A0XX012345");
    assert_eq!(info.paired_host_id, [1, 2, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn rejects_a_short_reply() {
    let data = support::discovery_reply(&support::DiscoveryConfig::default());
    assert!(parse_reply(&data[..131], Ipv4Addr::LOCALHOST).is_none());
}

#[test]
fn rejects_a_reply_from_a_different_sender() {
    let data = support::discovery_reply(&support::DiscoveryConfig::default());
    assert!(parse_reply(&data, Ipv4Addr::new(192, 168, 0, 130)).is_none());
}
