use std::net::IpAddr;

use nntune_ssrf_guard::{ip_is_forbidden, v4_is_forbidden};

#[test]
fn ipv4_special_use_ranges_are_inclusive() {
    for (first, last) in [
        ("0.0.0.0", "0.255.255.255"),
        ("10.0.0.0", "10.255.255.255"),
        ("100.64.0.0", "100.127.255.255"),
        ("127.0.0.0", "127.255.255.255"),
        ("169.254.0.0", "169.254.255.255"),
        ("172.16.0.0", "172.31.255.255"),
        ("192.88.99.0", "192.88.99.255"),
        ("192.168.0.0", "192.168.255.255"),
        ("198.18.0.0", "198.19.255.255"),
        ("224.0.0.0", "239.255.255.255"),
        ("240.0.0.0", "255.255.255.255"),
    ] {
        for raw in [first, last] {
            let ip = raw.parse().unwrap();
            assert!(ip_is_forbidden(&IpAddr::V4(ip)), "{raw}");
            assert!(v4_is_forbidden(&ip.octets()), "{raw}");
        }
    }
    for raw in [
        "100.63.255.255",
        "100.128.0.0",
        "169.253.255.255",
        "169.255.0.0",
        "172.15.255.255",
        "172.32.0.0",
        "192.88.98.255",
        "192.88.100.0",
        "223.255.255.255",
    ] {
        assert!(!ip_is_forbidden(&raw.parse().unwrap()), "{raw}");
    }
}

#[test]
fn ipv6_nonpublic_and_transition_addresses_stay_blocked() {
    for raw in [
        "::",
        "::1",
        "::ffff:10.0.0.1",
        "::ffff:8.8.8.8",
        "::ffff:0:808:808",
        "::8.8.8.8",
        "64:ff9b:1::808:808",
        "2002:808:808::1",
        "3fff:fff:ffff:ffff:ffff:ffff:ffff:ffff",
        "fc00::",
        "fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
        "fe80::",
        "febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
        "fec0::1",
        "ff02::1",
    ] {
        assert!(ip_is_forbidden(&raw.parse().unwrap()), "{raw}");
    }
    assert!(!ip_is_forbidden(&"2001:4860:4860::8888".parse().unwrap()));
}
