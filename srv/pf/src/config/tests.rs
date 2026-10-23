use super::*;

use pretty_assertions::assert_eq;

#[test]
fn network_proxy_settings_default_matches_local_use_baseline() {
    assert_eq!(
        NetworkProxySettings::default(),
        NetworkProxySettings {
            enabled: false,
            proxy_url: "http://127.0.0.1:3128".to_string(),
            enable_socks5: true,
            socks_url: "http://127.0.0.1:8081".to_string(),
            enable_socks5_udp: true,
            allow_upstream_proxy: true,
            dangerously_allow_non_loopback_proxy: false,
            dangerously_allow_all_unix_sockets: false,
            mode: NetworkMode::Full,
            allowed_domains: Vec::new(),
            denied_domains: Vec::new(),
            allow_unix_sockets: Vec::new(),
            allow_local_binding: false,
            mitm: false,
        }
    );
}

#[test]
fn partial_network_config_uses_struct_defaults_for_missing_fields() {
    let config: NetworkProxyConfig = serde_json::from_str(
        r#"{
                "network": {
                    "enabled": true
                }
            }"#,
    )
    .unwrap();
    let expected = NetworkProxySettings {
        enabled: true,
        ..NetworkProxySettings::default()
    };

    assert_eq!(config.network, expected);
}

#[test]
fn parse_host_port_preserves_address_normalization_and_default_ports() {
    for (input, host, port) in [
        ("127.0.0.1:8080", "127.0.0.1", 8080),
        ("http://example.com:8080/some/path", "example.com", 8080),
        ("http://user:pass@host.example:5555", "host.example", 5555),
        ("http://[::1]:9999", "::1", 9999),
        ("2001:db8::1", "2001:db8::1", 3128),
        ("example.com:notaport", "example.com", 3128),
    ] {
        assert_eq!(
            parse_host_port(input, 3128).unwrap(),
            SocketAddressParts {
                host: host.to_string(),
                port,
            },
            "{input}"
        );
    }
    for (input, default_port) in [("", 1234), ("   ", 5555)] {
        assert!(parse_host_port(input, default_port).is_err(), "{input:?}");
    }
}

#[test]
fn host_and_port_from_network_addr_defaults_for_empty_string() {
    assert_eq!(host_and_port_from_network_addr("", 1234), "<missing>");
}

#[test]
fn host_and_port_from_network_addr_formats_ipv6() {
    assert_eq!(
        host_and_port_from_network_addr("http://[::1]:8080", 3128),
        "[::1]:8080"
    );
}

#[test]
fn resolve_addr_preserves_literals_and_binds_hostnames_to_loopback() {
    for (input, default_port, expected) in [
        ("localhost", 3128, "127.0.0.1:3128"),
        ("1.2.3.4", 80, "1.2.3.4:80"),
        ("http://[::1]:8080", 3128, "[::1]:8080"),
        ("http://example.com:5555", 3128, "127.0.0.1:5555"),
    ] {
        assert_eq!(
            resolve_addr(input, default_port).unwrap(),
            expected.parse::<SocketAddr>().unwrap(),
            "{input}"
        );
    }
}

#[test]
fn unix_socket_access_forces_loopback_even_with_non_loopback_opt_in() {
    for (label, allow_unix_sockets, allow_all, expected_host) in [
        ("TCP only", vec![], false, "0.0.0.0"),
        (
            "allowlisted socket",
            vec!["/tmp/docker.sock".to_string()],
            false,
            "127.0.0.1",
        ),
        ("all sockets", vec![], true, "127.0.0.1"),
    ] {
        let cfg = NetworkProxySettings {
            dangerously_allow_non_loopback_proxy: true,
            allow_unix_sockets,
            dangerously_allow_all_unix_sockets: allow_all,
            ..Default::default()
        };
        let (http_addr, socks_addr) = clamp_bind_addrs(
            "0.0.0.0:3128".parse().unwrap(),
            "0.0.0.0:8081".parse().unwrap(),
            &cfg,
        );
        assert_eq!(
            http_addr,
            format!("{expected_host}:3128")
                .parse::<SocketAddr>()
                .unwrap(),
            "{label}"
        );
        assert_eq!(
            socks_addr,
            format!("{expected_host}:8081")
                .parse::<SocketAddr>()
                .unwrap(),
            "{label}"
        );
    }
}

#[test]
fn resolve_runtime_rejects_relative_allow_unix_sockets_entries() {
    let cfg = NetworkProxyConfig {
        network: NetworkProxySettings {
            allow_unix_sockets: vec!["relative.sock".to_string()],
            ..NetworkProxySettings::default()
        },
    };

    let err = match resolve_runtime(&cfg) {
        Ok(runtime) => panic!(
            "relative allow_unix_sockets should fail, but resolve_runtime succeeded: {:?}",
            runtime.http_addr
        ),
        Err(err) => err,
    };
    assert!(
        err.to_string().contains("network.allow_unix_sockets[0]"),
        "error should point at the invalid allow_unix_sockets entry: {err:#}"
    );
}

#[test]
fn resolve_runtime_accepts_unix_style_absolute_allow_unix_sockets_entries() {
    let cfg = NetworkProxyConfig {
        network: NetworkProxySettings {
            allow_unix_sockets: vec!["/private/tmp/example.sock".to_string()],
            ..NetworkProxySettings::default()
        },
    };

    assert!(
        resolve_runtime(&cfg).is_ok(),
        "unix-style absolute allow_unix_sockets entry should be accepted"
    );
}
