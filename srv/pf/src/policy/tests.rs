use super::*;

use pretty_assertions::assert_eq;

#[test]
fn method_allowed_full_allows_everything() {
    assert!(NetworkMode::Full.allows_method("GET"));
    assert!(NetworkMode::Full.allows_method("POST"));
    assert!(NetworkMode::Full.allows_method("CONNECT"));
}

#[test]
fn method_allowed_limited_allows_only_safe_methods() {
    assert!(NetworkMode::Limited.allows_method("GET"));
    assert!(NetworkMode::Limited.allows_method("HEAD"));
    assert!(NetworkMode::Limited.allows_method("OPTIONS"));
    assert!(!NetworkMode::Limited.allows_method("POST"));
    assert!(!NetworkMode::Limited.allows_method("CONNECT"));
}

#[test]
fn compile_globset_normalizes_patterns_and_preserves_host_scope() {
    for (label, pattern, matches) in [
        (
            "exact host with trailing dot",
            "Example.COM.",
            [("example.com", true), ("api.example.com", false)].as_slice(),
        ),
        (
            "subdomains only",
            "*.Example.COM.",
            [("api.example.com", true), ("example.com", false)].as_slice(),
        ),
        (
            "apex and subdomains",
            "**.Example.COM.",
            [("example.com", true), ("api.example.com", true)].as_slice(),
        ),
        ("bracketed IPv6", "[::1]", [("::1", true)].as_slice()),
    ] {
        let set = compile_globset(&[pattern.to_string()]).expect(label);
        for &(host, expected) in matches {
            assert_eq!(set.is_match(host), expected, "{label}: {host}");
        }
    }
}

#[test]
fn is_loopback_host_handles_localhost_variants() {
    assert!(is_loopback_host(&Host::parse("localhost").unwrap()));
    assert!(is_loopback_host(&Host::parse("localhost.").unwrap()));
    assert!(is_loopback_host(&Host::parse("LOCALHOST").unwrap()));
    assert!(!is_loopback_host(&Host::parse("notlocalhost").unwrap()));
}

#[test]
fn is_loopback_host_handles_ip_literals() {
    assert!(is_loopback_host(&Host::parse("127.0.0.1").unwrap()));
    assert!(is_loopback_host(&Host::parse("::1").unwrap()));
    assert!(!is_loopback_host(&Host::parse("1.2.3.4").unwrap()));
}

#[test]
fn is_non_public_ip_rejects_private_and_loopback_ranges() {
    assert!(is_non_public_ip("127.0.0.1".parse().unwrap()));
    assert!(is_non_public_ip("10.0.0.1".parse().unwrap()));
    assert!(is_non_public_ip("192.168.0.1".parse().unwrap()));
    assert!(is_non_public_ip("100.64.0.1".parse().unwrap()));
    assert!(is_non_public_ip("192.0.0.1".parse().unwrap()));
    assert!(is_non_public_ip("192.0.2.1".parse().unwrap()));
    assert!(is_non_public_ip("198.18.0.1".parse().unwrap()));
    assert!(is_non_public_ip("198.51.100.1".parse().unwrap()));
    assert!(is_non_public_ip("203.0.113.1".parse().unwrap()));
    assert!(is_non_public_ip("240.0.0.1".parse().unwrap()));
    assert!(is_non_public_ip("0.1.2.3".parse().unwrap()));
    assert!(!is_non_public_ip("8.8.8.8".parse().unwrap()));

    assert!(is_non_public_ip("::ffff:127.0.0.1".parse().unwrap()));
    assert!(is_non_public_ip("::ffff:10.0.0.1".parse().unwrap()));
    assert!(!is_non_public_ip("::ffff:8.8.8.8".parse().unwrap()));

    assert!(is_non_public_ip("::1".parse().unwrap()));
    assert!(is_non_public_ip("fe80::1".parse().unwrap()));
    assert!(is_non_public_ip("fc00::1".parse().unwrap()));
}

#[test]
fn normalize_host_preserves_addresses_while_removing_host_decoration() {
    for (label, input, expected) in [
        ("case and whitespace", "  ExAmPlE.CoM  ", "example.com"),
        ("host port", "example.com:1234", "example.com"),
        ("unbracketed IPv6", "2001:db8::1", "2001:db8::1"),
        ("trailing dot", "example.com.", "example.com"),
        ("case and trailing dot", "ExAmPlE.CoM.", "example.com"),
        ("trailing dot with port", "example.com.:443", "example.com"),
        ("bracketed IPv6", "[::1]", "::1"),
        ("bracketed IPv6 with port", "[::1]:443", "::1"),
    ] {
        assert_eq!(normalize_host(input), expected, "{label}: {input}");
    }
}
