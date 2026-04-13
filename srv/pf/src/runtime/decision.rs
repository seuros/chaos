use crate::policy::Host;
use crate::policy::is_loopback_host;
use crate::policy::is_non_public_ip;
use crate::policy::normalize_host;
use crate::reasons::REASON_DENIED;
use crate::reasons::REASON_NOT_ALLOWED;
use crate::reasons::REASON_NOT_ALLOWED_LOCAL;
use crate::runtime::helpers::host_resolves_to_non_public_ip;
use globset::GlobSet;
use std::net::IpAddr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostBlockReason {
    Denied,
    NotAllowed,
    NotAllowedLocal,
}

impl HostBlockReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Denied => REASON_DENIED,
            Self::NotAllowed => REASON_NOT_ALLOWED,
            Self::NotAllowedLocal => REASON_NOT_ALLOWED_LOCAL,
        }
    }
}

impl std::fmt::Display for HostBlockReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostBlockDecision {
    Allowed,
    Blocked(HostBlockReason),
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn evaluate_host_block(
    host_str: &str,
    port: u16,
    deny_set: &GlobSet,
    allow_set: &GlobSet,
    allow_local_binding: bool,
    allowed_domains_empty: bool,
    allowed_domains: &[String],
    host: &Host,
) -> HostBlockDecision {
    // Decision order matters:
    //  1) explicit deny always wins
    //  2) local/private networking is opt-in (defense-in-depth)
    //  3) allowlist is enforced when configured
    if deny_set.is_match(host_str) {
        return HostBlockDecision::Blocked(HostBlockReason::Denied);
    }

    let is_allowlisted = allow_set.is_match(host_str);
    if !allow_local_binding {
        // If the intent is "prevent access to local/internal networks", we must not rely solely
        // on string checks like `localhost` / `127.0.0.1`. Attackers can use DNS rebinding or
        // public suffix services that map hostnames onto private IPs.
        //
        // We therefore do a best-effort DNS + IP classification check before allowing the
        // request. Explicit local/loopback literals are allowed only when explicitly
        // allowlisted; hostnames that resolve to local/private IPs are blocked even if
        // allowlisted.
        let local_literal = {
            let host_no_scope = host_str
                .split_once('%')
                .map(|(ip, _)| ip)
                .unwrap_or(host_str);
            if is_loopback_host(host) {
                true
            } else if let Ok(ip) = host_no_scope.parse::<IpAddr>() {
                is_non_public_ip(ip)
            } else {
                false
            }
        };

        if local_literal {
            if !is_explicit_local_allowlisted(allowed_domains, host) {
                return HostBlockDecision::Blocked(HostBlockReason::NotAllowedLocal);
            }
        } else if host_resolves_to_non_public_ip(host_str, port).await {
            return HostBlockDecision::Blocked(HostBlockReason::NotAllowedLocal);
        }
    }

    if allowed_domains_empty || !is_allowlisted {
        HostBlockDecision::Blocked(HostBlockReason::NotAllowed)
    } else {
        HostBlockDecision::Allowed
    }
}

pub(super) fn is_explicit_local_allowlisted(allowed_domains: &[String], host: &Host) -> bool {
    let normalized_host = host.as_str();
    allowed_domains.iter().any(|pattern| {
        let pattern = pattern.trim();
        if pattern == "*" || pattern.starts_with("*.") || pattern.starts_with("**.") {
            return false;
        }
        if pattern.contains('*') || pattern.contains('?') {
            return false;
        }
        normalize_host(pattern) == normalized_host
    })
}
