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
fn compile_globset_normalizes_trailing_dots() {
    let set = compile_globset(&["Example.COM.".to_string()]).unwrap();

    assert_eq!(true, set.is_match("example.com"));
    assert_eq!(false, set.is_match("api.example.com"));
}

#[test]
fn compile_globset_normalizes_wildcards() {
    let set = compile_globset(&["*.Example.COM.".to_string()]).unwrap();

    assert_eq!(true, set.is_match("api.example.com"));
    assert_eq!(false, set.is_match("example.com"));
}

#[test]
fn compile_globset_normalizes_apex_and_subdomains() {
    let set = compile_globset(&["**.Example.COM.".to_string()]).unwrap();

    assert_eq!(true, set.is_match("example.com"));
    assert_eq!(true, set.is_match("api.example.com"));
}

#[test]
fn compile_globset_normalizes_bracketed_ipv6_literals() {
    let set = compile_globset(&["[::1]".to_string()]).unwrap();

    assert_eq!(true, set.is_match("::1"));
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
fn normalize_host_lowercases_and_trims() {
    assert_eq!(normalize_host("  ExAmPlE.CoM  "), "example.com");
}

#[test]
fn normalize_host_strips_port_for_host_port() {
    assert_eq!(normalize_host("example.com:1234"), "example.com");
}

#[test]
fn normalize_host_preserves_unbracketed_ipv6() {
    assert_eq!(normalize_host("2001:db8::1"), "2001:db8::1");
}

#[test]
fn normalize_host_strips_trailing_dot() {
    assert_eq!(normalize_host("example.com."), "example.com");
    assert_eq!(normalize_host("ExAmPlE.CoM."), "example.com");
}

#[test]
fn normalize_host_strips_trailing_dot_with_port() {
    assert_eq!(normalize_host("example.com.:443"), "example.com");
}

#[test]
fn normalize_host_strips_brackets_for_ipv6() {
    assert_eq!(normalize_host("[::1]"), "::1");
    assert_eq!(normalize_host("[::1]:443"), "::1");
}
