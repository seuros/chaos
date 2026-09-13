use super::*;
use crate::config::NetworkProxySettings;
use crate::state::network_proxy_state_for_policy;
use pretty_assertions::assert_eq;
use std::net::IpAddr;
use std::net::Ipv4Addr;

#[tokio::test]
async fn managed_proxy_builder_uses_loopback_ephemeral_ports() {
    let state = Arc::new(network_proxy_state_for_policy(
        NetworkProxySettings::default(),
    ));
    let proxy = match NetworkProxy::builder().state(state).build().await {
        Ok(proxy) => proxy,
        Err(err) => {
            if err
                .chain()
                .any(|cause| cause.to_string().contains("Operation not permitted"))
            {
                return;
            }
            panic!("failed to build managed proxy: {err:#}");
        }
    };

    assert!(proxy.http_addr.ip().is_loopback());
    assert!(proxy.socks_addr.ip().is_loopback());
    assert_ne!(proxy.http_addr.port(), 0);
    assert_ne!(proxy.socks_addr.port(), 0);
}

#[tokio::test]
async fn non_codex_managed_proxy_builder_uses_configured_ports() {
    let settings = NetworkProxySettings {
        proxy_url: "http://127.0.0.1:43128".to_string(),
        socks_url: "http://127.0.0.1:48081".to_string(),
        ..NetworkProxySettings::default()
    };
    let state = Arc::new(network_proxy_state_for_policy(settings));
    let proxy = NetworkProxy::builder()
        .state(state)
        .managed_by_codex(false)
        .build()
        .await
        .unwrap();

    assert_eq!(
        proxy.http_addr,
        "127.0.0.1:43128".parse::<SocketAddr>().unwrap()
    );
    assert_eq!(
        proxy.socks_addr,
        "127.0.0.1:48081".parse::<SocketAddr>().unwrap()
    );
}

#[tokio::test]
async fn managed_proxy_builder_does_not_reserve_socks_listener_when_disabled() {
    let settings = NetworkProxySettings {
        enable_socks5: false,
        socks_url: "http://127.0.0.1:43129".to_string(),
        ..NetworkProxySettings::default()
    };
    let state = Arc::new(network_proxy_state_for_policy(settings));
    let proxy = match NetworkProxy::builder().state(state).build().await {
        Ok(proxy) => proxy,
        Err(err) => {
            if err
                .chain()
                .any(|cause| cause.to_string().contains("Operation not permitted"))
            {
                return;
            }
            panic!("failed to build managed proxy: {err:#}");
        }
    };

    assert!(proxy.http_addr.ip().is_loopback());
    assert_eq!(
        proxy.socks_addr,
        "127.0.0.1:43129".parse::<SocketAddr>().unwrap()
    );
    assert!(
        proxy
            .reserved_listeners
            .as_ref()
            .expect("managed builder should reserve listeners")
            .take_socks()
            .is_none()
    );
}

#[test]
fn proxy_url_env_value_resolves_lowercase_aliases() {
    let mut env = HashMap::new();
    env.insert(
        "http_proxy".to_string(),
        "http://127.0.0.1:3128".to_string(),
    );

    assert_eq!(
        proxy_url_env_value(&env, "HTTP_PROXY"),
        Some("http://127.0.0.1:3128")
    );
}

#[test]
fn has_proxy_url_env_vars_detects_lowercase_aliases() {
    let mut env = HashMap::new();
    env.insert(
        "all_proxy".to_string(),
        "socks5h://127.0.0.1:8081".to_string(),
    );

    assert_eq!(has_proxy_url_env_vars(&env), true);
}

#[test]
fn has_proxy_url_env_vars_detects_websocket_proxy_keys() {
    let mut env = HashMap::new();
    env.insert("wss_proxy".to_string(), "http://127.0.0.1:3128".to_string());

    assert_eq!(has_proxy_url_env_vars(&env), true);
}

#[test]
fn apply_proxy_env_overrides_sets_common_tool_vars() {
    let mut env = HashMap::new();
    apply_proxy_env_overrides(
        &mut env,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3128),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8081),
        true,
        false,
    );

    assert_eq!(
        env.get("HTTP_PROXY"),
        Some(&"http://127.0.0.1:3128".to_string())
    );
    assert_eq!(
        env.get("WS_PROXY"),
        Some(&"http://127.0.0.1:3128".to_string())
    );
    assert_eq!(
        env.get("WSS_PROXY"),
        Some(&"http://127.0.0.1:3128".to_string())
    );
    assert_eq!(
        env.get("npm_config_proxy"),
        Some(&"http://127.0.0.1:3128".to_string())
    );
    assert_eq!(
        env.get("ALL_PROXY"),
        Some(&"socks5h://127.0.0.1:8081".to_string())
    );
    assert_eq!(
        env.get("FTP_PROXY"),
        Some(&"socks5h://127.0.0.1:8081".to_string())
    );
    assert_eq!(
        env.get("NO_PROXY"),
        Some(&DEFAULT_NO_PROXY_VALUE.to_string())
    );
    assert_eq!(env.get(ALLOW_LOCAL_BINDING_ENV_KEY), Some(&"0".to_string()));
    assert_eq!(env.get("ELECTRON_GET_USE_PROXY"), Some(&"true".to_string()));
    #[cfg(target_os = "macos")]
    assert_eq!(
        env.get("GIT_SSH_COMMAND"),
        Some(&"ssh -o ProxyCommand='nc -X 5 -x 127.0.0.1:8081 %h %p'".to_string())
    );
    #[cfg(not(target_os = "macos"))]
    assert_eq!(env.get("GIT_SSH_COMMAND"), None);
}

#[test]
fn apply_proxy_env_overrides_uses_http_for_all_proxy_without_socks() {
    let mut env = HashMap::new();
    apply_proxy_env_overrides(
        &mut env,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3128),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8081),
        false,
        true,
    );

    assert_eq!(
        env.get("ALL_PROXY"),
        Some(&"http://127.0.0.1:3128".to_string())
    );
    assert_eq!(env.get(ALLOW_LOCAL_BINDING_ENV_KEY), Some(&"1".to_string()));
}

#[test]
fn apply_proxy_env_overrides_uses_plain_http_proxy_url() {
    let mut env = HashMap::new();
    apply_proxy_env_overrides(
        &mut env,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3128),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8081),
        true,
        false,
    );

    assert_eq!(
        env.get("HTTP_PROXY"),
        Some(&"http://127.0.0.1:3128".to_string())
    );
    assert_eq!(
        env.get("HTTPS_PROXY"),
        Some(&"http://127.0.0.1:3128".to_string())
    );
    assert_eq!(
        env.get("WS_PROXY"),
        Some(&"http://127.0.0.1:3128".to_string())
    );
    assert_eq!(
        env.get("WSS_PROXY"),
        Some(&"http://127.0.0.1:3128".to_string())
    );
    assert_eq!(
        env.get("ALL_PROXY"),
        Some(&"socks5h://127.0.0.1:8081".to_string())
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        env.get("GIT_SSH_COMMAND"),
        Some(&"ssh -o ProxyCommand='nc -X 5 -x 127.0.0.1:8081 %h %p'".to_string())
    );
    #[cfg(not(target_os = "macos"))]
    assert_eq!(env.get("GIT_SSH_COMMAND"), None);
}

#[cfg(target_os = "macos")]
#[test]
fn apply_proxy_env_overrides_preserves_existing_git_ssh_command() {
    let mut env = HashMap::new();
    env.insert(
        "GIT_SSH_COMMAND".to_string(),
        "ssh -o ProxyCommand='tsh proxy ssh --cluster=dev %r@%h:%p'".to_string(),
    );
    apply_proxy_env_overrides(
        &mut env,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3128),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8081),
        true,
        false,
    );

    assert_eq!(
        env.get("GIT_SSH_COMMAND"),
        Some(&"ssh -o ProxyCommand='tsh proxy ssh --cluster=dev %r@%h:%p'".to_string())
    );
}
