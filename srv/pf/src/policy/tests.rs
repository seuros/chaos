use super::*;

use pretty_assertions::assert_eq;

#[test]
fn network_modes_limit_methods_without_restricting_full_mode() {
    for (method, limited) in [
        ("GET", true),
        ("HEAD", true),
        ("OPTIONS", true),
        ("POST", false),
        ("CONNECT", false),
    ] {
        assert!(NetworkMode::Full.allows_method(method), "{method}");
        assert_eq!(
            NetworkMode::Limited.allows_method(method),
            limited,
            "{method}"
        );
    }
}

#[test]
fn compile_globset_normalizes_patterns_and_preserves_host_scope() {
    for (label, pattern, matches) in [
        (
            "exact host with trailing dot",
            "Example.COM.",
            [
                ("example.com", true),
                ("EXAMPLE.COM", true),
                ("api.example.com", false),
            ]
            .as_slice(),
        ),
        (
            "subdomains only",
            "*.Example.COM.",
            [
                ("api.example.com", true),
                ("example.com", false),
                ("evilexample.com", false),
            ]
            .as_slice(),
        ),
        (
            "apex and subdomains",
            "**.Example.COM.",
            [
                ("example.com", true),
                ("api.example.com", true),
                ("evilexample.com", false),
            ]
            .as_slice(),
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
fn loopback_detection_accepts_local_names_and_literals_not_lookalikes() {
    for (host, expected) in [
        ("localhost", true),
        ("localhost.", true),
        ("LOCALHOST", true),
        ("notlocalhost", false),
        ("127.0.0.1", true),
        ("::1", true),
        ("1.2.3.4", false),
    ] {
        assert_eq!(
            is_loopback_host(&Host::parse(host).unwrap()),
            expected,
            "{host}"
        );
    }
}

#[test]
fn is_non_public_ip_rejects_private_and_loopback_ranges() {
    for (ip, expected) in [
        ("127.0.0.1", true),
        ("10.0.0.1", true),
        ("192.168.0.1", true),
        ("100.64.0.1", true),
        ("192.0.0.1", true),
        ("192.0.2.1", true),
        ("198.18.0.1", true),
        ("198.51.100.1", true),
        ("203.0.113.1", true),
        ("240.0.0.1", true),
        ("0.1.2.3", true),
        ("8.8.8.8", false),
        ("::ffff:127.0.0.1", true),
        ("::ffff:10.0.0.1", true),
        ("::ffff:8.8.8.8", false),
        ("::1", true),
        ("fe80::1", true),
        ("fc00::1", true),
    ] {
        assert_eq!(is_non_public_ip(ip.parse().unwrap()), expected, "{ip}");
    }
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
