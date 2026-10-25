use super::*;

#[test]
fn peer_normalization_unmaps_only_ipv4_mapped_ipv6() {
    let mapped: IpAddr = "::ffff:192.168.12.34".parse().unwrap();
    assert_eq!(
        normalize_peer_ip(mapped),
        "192.168.12.34".parse::<IpAddr>().unwrap()
    );
    let loopback: IpAddr = "::1".parse().unwrap();
    assert_eq!(normalize_peer_ip(loopback), loopback);
    let link_local: IpAddr = "fe80::1".parse().unwrap();
    assert_eq!(normalize_peer_ip(link_local), link_local);
}

#[test]
fn peer_allowlist_covers_only_loopback_and_the_192_168_16_range() {
    for allowed in [
        "127.0.0.0",
        "127.255.255.255",
        "::1",
        "192.168.0.0",
        "192.168.255.255",
        "::ffff:127.9.8.7",
        "::ffff:192.168.14.22",
    ] {
        let peer = allowed.parse().unwrap();
        assert!(allowed_peer_ip(peer), "{allowed}");
    }

    for rejected in [
        "0.0.0.0",
        "10.0.0.1",
        "172.16.0.1",
        "172.31.255.254",
        "192.167.255.255",
        "192.169.0.0",
        "169.254.0.1",
        "8.8.8.8",
        "::",
        "fe80::1",
        "fc00::1",
        "::ffff:10.0.0.1",
    ] {
        let peer = rejected.parse().unwrap();
        assert!(!allowed_peer_ip(peer), "{rejected}");
    }
}
