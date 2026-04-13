use crate::config::NetworkProxyConfig;
use crate::policy::is_non_public_ip;
use std::collections::HashSet;
use std::net::IpAddr;
use std::time::Duration;
use tokio::net::lookup_host;
use tokio::time::timeout;
use tracing::info;

pub(super) const DNS_LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkProxyAuditMetadata {
    pub conversation_id: Option<String>,
    pub app_version: Option<String>,
    pub auth_mode: Option<String>,
    pub originator: Option<String>,
    pub terminal_type: Option<String>,
    pub model: Option<String>,
    pub slug: Option<String>,
}

pub(crate) fn unix_socket_permissions_supported() -> bool {
    cfg!(target_os = "macos")
}

pub(super) async fn host_resolves_to_non_public_ip(host: &str, port: u16) -> bool {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_non_public_ip(ip);
    }

    // If DNS lookup fails, default to "not local/private" rather than blocking. In practice, the
    // subsequent connect attempt will fail anyway, and blocking on transient resolver issues would
    // make the proxy fragile. The allowlist/denylist remains the primary control plane.
    let addrs = match timeout(DNS_LOOKUP_TIMEOUT, lookup_host((host, port))).await {
        Ok(Ok(addrs)) => addrs,
        Ok(Err(_)) | Err(_) => return false,
    };

    for addr in addrs {
        if is_non_public_ip(addr.ip()) {
            return true;
        }
    }

    false
}

pub(super) fn log_policy_changes(previous: &NetworkProxyConfig, next: &NetworkProxyConfig) {
    log_domain_list_changes(
        "allowlist",
        &previous.network.allowed_domains,
        &next.network.allowed_domains,
    );
    log_domain_list_changes(
        "denylist",
        &previous.network.denied_domains,
        &next.network.denied_domains,
    );
}

fn log_domain_list_changes(list_name: &str, previous: &[String], next: &[String]) {
    let previous_set: HashSet<String> = previous
        .iter()
        .map(|entry| entry.to_ascii_lowercase())
        .collect();
    let next_set: HashSet<String> = next
        .iter()
        .map(|entry| entry.to_ascii_lowercase())
        .collect();

    let added = next_set
        .difference(&previous_set)
        .cloned()
        .collect::<HashSet<_>>();
    let removed = previous_set
        .difference(&next_set)
        .cloned()
        .collect::<HashSet<_>>();

    let mut seen_next = HashSet::new();
    for entry in next {
        let key = entry.to_ascii_lowercase();
        if seen_next.insert(key.clone()) && added.contains(&key) {
            info!("config entry added to {list_name}: {entry}");
        }
    }

    let mut seen_previous = HashSet::new();
    for entry in previous {
        let key = entry.to_ascii_lowercase();
        if seen_previous.insert(key.clone()) && removed.contains(&key) {
            info!("config entry removed from {list_name}: {entry}");
        }
    }
}
