use crate::config::NetworkProxyConfig;

#[derive(Clone, Copy)]
pub(super) enum DomainListKind {
    Allow,
    Deny,
}

impl DomainListKind {
    pub(super) fn list_name(self) -> &'static str {
        match self {
            Self::Allow => "allowlist",
            Self::Deny => "denylist",
        }
    }

    pub(super) fn constraint_field(self) -> &'static str {
        match self {
            Self::Allow => "network.allowed_domains",
            Self::Deny => "network.denied_domains",
        }
    }
}

impl NetworkProxyConfig {
    pub(super) fn split_domain_lists_mut(
        &mut self,
        target: DomainListKind,
    ) -> (&mut Vec<String>, &mut Vec<String>) {
        match target {
            DomainListKind::Allow => (
                &mut self.network.allowed_domains,
                &mut self.network.denied_domains,
            ),
            DomainListKind::Deny => (
                &mut self.network.denied_domains,
                &mut self.network.allowed_domains,
            ),
        }
    }
}
