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
fn parse_host_port_defaults_for_empty_string() {
    assert!(parse_host_port("", 1234).is_err());
}

#[test]
fn parse_host_port_defaults_for_whitespace() {
    assert!(parse_host_port("   ", 5555).is_err());
}

#[test]
fn parse_host_port_parses_host_port_without_scheme() {
    assert_eq!(
        parse_host_port("127.0.0.1:8080", 3128).unwrap(),
        SocketAddressParts {
            host: "127.0.0.1".to_string(),
            port: 8080,
        }
    );
}

#[test]
fn parse_host_port_parses_host_port_with_scheme_and_path() {
    assert_eq!(
        parse_host_port("http://example.com:8080/some/path", 3128).unwrap(),
        SocketAddressParts {
            host: "example.com".to_string(),
            port: 8080,
        }
    );
}

#[test]
fn parse_host_port_strips_userinfo() {
    assert_eq!(
        parse_host_port("http://user:pass@host.example:5555", 3128).unwrap(),
        SocketAddressParts {
            host: "host.example".to_string(),
            port: 5555,
        }
    );
}

#[test]
fn parse_host_port_parses_ipv6_with_brackets() {
    assert_eq!(
        parse_host_port("http://[::1]:9999", 3128).unwrap(),
        SocketAddressParts {
            host: "::1".to_string(),
            port: 9999,
        }
    );
}

#[test]
fn parse_host_port_does_not_treat_unbracketed_ipv6_as_host_port() {
    assert_eq!(
        parse_host_port("2001:db8::1", 3128).unwrap(),
        SocketAddressParts {
            host: "2001:db8::1".to_string(),
            port: 3128,
        }
    );
}

#[test]
fn parse_host_port_falls_back_to_default_port_when_port_is_invalid() {
    assert_eq!(
        parse_host_port("example.com:notaport", 3128).unwrap(),
        SocketAddressParts {
            host: "example.com".to_string(),
            port: 3128,
        }
    );
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
fn resolve_addr_maps_localhost_to_loopback() {
    assert_eq!(
        resolve_addr("localhost", 3128).unwrap(),
        "127.0.0.1:3128".parse::<SocketAddr>().unwrap()
    );
}

#[test]
fn resolve_addr_parses_ip_literals() {
    assert_eq!(
        resolve_addr("1.2.3.4", 80).unwrap(),
        "1.2.3.4:80".parse::<SocketAddr>().unwrap()
    );
}

#[test]
fn resolve_addr_parses_ipv6_literals() {
    assert_eq!(
        resolve_addr("http://[::1]:8080", 3128).unwrap(),
        "[::1]:8080".parse::<SocketAddr>().unwrap()
    );
}

#[test]
fn resolve_addr_falls_back_to_loopback_for_hostnames() {
    assert_eq!(
        resolve_addr("http://example.com:5555", 3128).unwrap(),
        "127.0.0.1:5555".parse::<SocketAddr>().unwrap()
    );
}

#[test]
fn clamp_bind_addrs_allows_non_loopback_when_enabled() {
    let cfg = NetworkProxySettings {
        dangerously_allow_non_loopback_proxy: true,
        ..Default::default()
    };
    let http_addr = "0.0.0.0:3128".parse::<SocketAddr>().unwrap();
    let socks_addr = "0.0.0.0:8081".parse::<SocketAddr>().unwrap();

    let (http_addr, socks_addr) = clamp_bind_addrs(http_addr, socks_addr, &cfg);

    assert_eq!(http_addr, "0.0.0.0:3128".parse::<SocketAddr>().unwrap());
    assert_eq!(socks_addr, "0.0.0.0:8081".parse::<SocketAddr>().unwrap());
}

#[test]
fn clamp_bind_addrs_forces_loopback_when_unix_sockets_enabled() {
    let cfg = NetworkProxySettings {
        dangerously_allow_non_loopback_proxy: true,
        allow_unix_sockets: vec!["/tmp/docker.sock".to_string()],
        ..Default::default()
    };
    let http_addr = "0.0.0.0:3128".parse::<SocketAddr>().unwrap();
    let socks_addr = "0.0.0.0:8081".parse::<SocketAddr>().unwrap();

    let (http_addr, socks_addr) = clamp_bind_addrs(http_addr, socks_addr, &cfg);

    assert_eq!(http_addr, "127.0.0.1:3128".parse::<SocketAddr>().unwrap());
    assert_eq!(socks_addr, "127.0.0.1:8081".parse::<SocketAddr>().unwrap());
}

#[test]
fn clamp_bind_addrs_forces_loopback_when_all_unix_sockets_enabled() {
    let cfg = NetworkProxySettings {
        dangerously_allow_non_loopback_proxy: true,
        dangerously_allow_all_unix_sockets: true,
        ..Default::default()
    };
    let http_addr = "0.0.0.0:3128".parse::<SocketAddr>().unwrap();
    let socks_addr = "0.0.0.0:8081".parse::<SocketAddr>().unwrap();

    let (http_addr, socks_addr) = clamp_bind_addrs(http_addr, socks_addr, &cfg);

    assert_eq!(http_addr, "127.0.0.1:3128".parse::<SocketAddr>().unwrap());
    assert_eq!(socks_addr, "127.0.0.1:8081".parse::<SocketAddr>().unwrap());
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
