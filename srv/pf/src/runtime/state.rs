use crate::config::NetworkMode;
use crate::config::NetworkProxyConfig;
use crate::config::ValidatedUnixSocketPath;
use crate::mitm::MitmState;
use crate::policy::Host;
use crate::policy::normalize_host;
use crate::runtime::decision::HostBlockDecision;
use crate::runtime::decision::HostBlockReason;
use crate::runtime::decision::evaluate_host_block;
use crate::runtime::events::BlockedRequest;
use crate::runtime::events::BlockedRequestObserver;
use crate::runtime::events::MAX_BLOCKED_EVENTS;
use crate::runtime::events::blocked_request_violation_log_line;
use crate::runtime::helpers::NetworkProxyAuditMetadata;
use crate::runtime::helpers::log_policy_changes;
use crate::runtime::helpers::unix_socket_permissions_supported;
use crate::runtime::policy::DomainListKind;
use crate::state::NetworkProxyConstraintError;
use crate::state::NetworkProxyConstraints;
use crate::state::build_config_state;
use crate::state::validate_policy_against_constraints;
use anyhow::Context;
use anyhow::Result;
use chaos_realpath::AbsolutePathBuf;
use globset::GlobSet;
use std::collections::VecDeque;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;
use tracing::info;
use tracing::warn;

#[derive(Clone)]
pub struct ConfigState {
    pub config: NetworkProxyConfig,
    pub allow_set: GlobSet,
    pub deny_set: GlobSet,
    pub mitm: Option<Arc<MitmState>>,
    pub constraints: NetworkProxyConstraints,
    pub blocked: VecDeque<BlockedRequest>,
    pub blocked_total: u64,
}

pub trait ConfigReloader: Send + Sync {
    /// Human-readable description of where config is loaded from, for logs.
    fn source_label(&self) -> String;

    /// Return a freshly loaded state if a reload is needed; otherwise, return `None`.
    fn maybe_reload(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ConfigState>>> + Send + '_>>;

    /// Force a reload, regardless of whether a change was detected.
    fn reload_now(&self) -> Pin<Box<dyn Future<Output = Result<ConfigState>> + Send + '_>>;
}

pub struct NetworkProxyState {
    pub(super) state: Arc<RwLock<ConfigState>>,
    pub(super) reloader: Arc<dyn ConfigReloader>,
    pub(super) blocked_request_observer: Arc<RwLock<Option<Arc<dyn BlockedRequestObserver>>>>,
    pub(super) audit_metadata: NetworkProxyAuditMetadata,
}

impl rama::extensions::Extension for NetworkProxyState {}

impl std::fmt::Debug for NetworkProxyState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Avoid logging internal state (config contents, derived globsets, etc.) which can be noisy
        // and may contain sensitive paths.
        f.debug_struct("NetworkProxyState").finish_non_exhaustive()
    }
}

impl Clone for NetworkProxyState {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            reloader: self.reloader.clone(),
            blocked_request_observer: self.blocked_request_observer.clone(),
            audit_metadata: self.audit_metadata.clone(),
        }
    }
}

impl NetworkProxyState {
    pub fn with_reloader(state: ConfigState, reloader: Arc<dyn ConfigReloader>) -> Self {
        Self::with_reloader_and_audit_metadata(
            state,
            reloader,
            NetworkProxyAuditMetadata::default(),
        )
    }

    pub fn with_reloader_and_blocked_observer(
        state: ConfigState,
        reloader: Arc<dyn ConfigReloader>,
        blocked_request_observer: Option<Arc<dyn BlockedRequestObserver>>,
    ) -> Self {
        Self::with_reloader_and_audit_metadata_and_blocked_observer(
            state,
            reloader,
            NetworkProxyAuditMetadata::default(),
            blocked_request_observer,
        )
    }

    pub fn with_reloader_and_audit_metadata(
        state: ConfigState,
        reloader: Arc<dyn ConfigReloader>,
        audit_metadata: NetworkProxyAuditMetadata,
    ) -> Self {
        Self::with_reloader_and_audit_metadata_and_blocked_observer(
            state,
            reloader,
            audit_metadata,
            /*blocked_request_observer*/ None,
        )
    }

    pub fn with_reloader_and_audit_metadata_and_blocked_observer(
        state: ConfigState,
        reloader: Arc<dyn ConfigReloader>,
        audit_metadata: NetworkProxyAuditMetadata,
        blocked_request_observer: Option<Arc<dyn BlockedRequestObserver>>,
    ) -> Self {
        Self {
            state: Arc::new(RwLock::new(state)),
            reloader,
            blocked_request_observer: Arc::new(RwLock::new(blocked_request_observer)),
            audit_metadata,
        }
    }

    pub async fn set_blocked_request_observer(
        &self,
        blocked_request_observer: Option<Arc<dyn BlockedRequestObserver>>,
    ) {
        let mut observer = self.blocked_request_observer.write().await;
        *observer = blocked_request_observer;
    }

    pub fn audit_metadata(&self) -> &NetworkProxyAuditMetadata {
        &self.audit_metadata
    }

    pub async fn current_cfg(&self) -> Result<NetworkProxyConfig> {
        // Callers treat `NetworkProxyState` as a live view of policy. We reload-on-demand so edits
        // to `config.toml` (including Chaos-managed writes) take effect without a restart.
        self.reload_if_needed().await?;
        let guard = self.state.read().await;
        Ok(guard.config.clone())
    }

    pub async fn current_patterns(&self) -> Result<(Vec<String>, Vec<String>)> {
        self.reload_if_needed().await?;
        let guard = self.state.read().await;
        Ok((
            guard.config.network.allowed_domains.clone(),
            guard.config.network.denied_domains.clone(),
        ))
    }

    pub async fn enabled(&self) -> Result<bool> {
        self.reload_if_needed().await?;
        let guard = self.state.read().await;
        Ok(guard.config.network.enabled)
    }

    pub async fn force_reload(&self) -> Result<()> {
        let previous_cfg = {
            let guard = self.state.read().await;
            guard.config.clone()
        };

        match self.reloader.reload_now().await {
            Ok(mut new_state) => {
                // Policy changes are operationally sensitive; logging diffs makes changes traceable
                // without needing to dump full config blobs (which can include unrelated settings).
                log_policy_changes(&previous_cfg, &new_state.config);
                {
                    let mut guard = self.state.write().await;
                    new_state.blocked = guard.blocked.clone();
                    *guard = new_state;
                }
                let source = self.reloader.source_label();
                info!("reloaded config from {source}");
                Ok(())
            }
            Err(err) => {
                let source = self.reloader.source_label();
                warn!("failed to reload config from {source}: {err}; keeping previous config");
                Err(err)
            }
        }
    }

    pub async fn host_blocked(&self, host: &str, port: u16) -> Result<HostBlockDecision> {
        self.reload_if_needed().await?;
        let host = match Host::parse(host) {
            Ok(host) => host,
            Err(_) => return Ok(HostBlockDecision::Blocked(HostBlockReason::NotAllowed)),
        };
        let (deny_set, allow_set, allow_local_binding, allowed_domains_empty, allowed_domains) = {
            let guard = self.state.read().await;
            (
                guard.deny_set.clone(),
                guard.allow_set.clone(),
                guard.config.network.allow_local_binding,
                guard.config.network.allowed_domains.is_empty(),
                guard.config.network.allowed_domains.clone(),
            )
        };

        let host_str = host.as_str();
        Ok(evaluate_host_block(
            host_str,
            port,
            &deny_set,
            &allow_set,
            allow_local_binding,
            allowed_domains_empty,
            &allowed_domains,
            &host,
        )
        .await)
    }

    pub async fn record_blocked(&self, entry: BlockedRequest) -> Result<()> {
        self.reload_if_needed().await?;
        let blocked_for_observer = entry.clone();
        let blocked_request_observer = self.blocked_request_observer.read().await.clone();
        let violation_line = blocked_request_violation_log_line(&entry);
        let mut guard = self.state.write().await;
        let host = entry.host.clone();
        let reason = entry.reason.clone();
        let decision = entry.decision.clone();
        let source = entry.source.clone();
        let protocol = entry.protocol.clone();
        let port = entry.port;
        guard.blocked.push_back(entry);
        guard.blocked_total = guard.blocked_total.saturating_add(1);
        let total = guard.blocked_total;
        while guard.blocked.len() > MAX_BLOCKED_EVENTS {
            guard.blocked.pop_front();
        }
        debug!(
            "recorded blocked request telemetry (total={}, host={}, reason={}, decision={:?}, source={:?}, protocol={}, port={:?}, buffered={})",
            total,
            host,
            reason,
            decision,
            source,
            protocol,
            port,
            guard.blocked.len()
        );
        debug!("{violation_line}");
        drop(guard);

        if let Some(observer) = blocked_request_observer {
            observer.on_blocked_request(blocked_for_observer).await;
        }
        Ok(())
    }

    /// Returns a snapshot of buffered blocked-request entries without consuming them.
    pub async fn blocked_snapshot(&self) -> Result<Vec<BlockedRequest>> {
        self.reload_if_needed().await?;
        let guard = self.state.read().await;
        Ok(guard.blocked.iter().cloned().collect())
    }

    /// Drain and return the buffered blocked-request entries in FIFO order.
    pub async fn drain_blocked(&self) -> Result<Vec<BlockedRequest>> {
        self.reload_if_needed().await?;
        let blocked = {
            let mut guard = self.state.write().await;
            std::mem::take(&mut guard.blocked)
        };
        Ok(blocked.into_iter().collect())
    }

    pub async fn is_unix_socket_allowed(&self, path: &str) -> Result<bool> {
        self.reload_if_needed().await?;
        if !unix_socket_permissions_supported() {
            return Ok(false);
        }

        // We only support absolute unix socket paths (a relative path would be ambiguous with
        // respect to the proxy process's CWD and can lead to confusing allowlist behavior).
        let requested_path = Path::new(path);
        if !requested_path.is_absolute() {
            return Ok(false);
        }

        let guard = self.state.read().await;
        if guard.config.network.dangerously_allow_all_unix_sockets {
            return Ok(true);
        }

        // Normalize the path while keeping the absolute-path requirement explicit.
        let requested_abs = match AbsolutePathBuf::from_absolute_path(requested_path) {
            Ok(path) => path,
            Err(_) => return Ok(false),
        };
        let requested_canonical = std::fs::canonicalize(requested_abs.as_path()).ok();
        for allowed in &guard.config.network.allow_unix_sockets {
            let allowed_path = match ValidatedUnixSocketPath::parse(allowed) {
                Ok(ValidatedUnixSocketPath::Native(path)) => path,
                Ok(ValidatedUnixSocketPath::UnixStyleAbsolute(_)) => continue,
                Err(err) => {
                    warn!("ignoring invalid network.allow_unix_sockets entry at runtime: {err:#}");
                    continue;
                }
            };

            if allowed_path.as_path() == requested_abs.as_path() {
                return Ok(true);
            }

            // Best-effort canonicalization to reduce surprises with symlinks.
            // If canonicalization fails (e.g., socket not created yet), fall back to raw comparison.
            let Some(requested_canonical) = &requested_canonical else {
                continue;
            };
            if let Ok(allowed_canonical) = std::fs::canonicalize(allowed_path.as_path())
                && &allowed_canonical == requested_canonical
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub async fn method_allowed(&self, method: &str) -> Result<bool> {
        self.reload_if_needed().await?;
        let guard = self.state.read().await;
        Ok(guard.config.network.mode.allows_method(method))
    }

    pub async fn allow_upstream_proxy(&self) -> Result<bool> {
        self.reload_if_needed().await?;
        let guard = self.state.read().await;
        Ok(guard.config.network.allow_upstream_proxy)
    }

    pub async fn network_mode(&self) -> Result<NetworkMode> {
        self.reload_if_needed().await?;
        let guard = self.state.read().await;
        Ok(guard.config.network.mode)
    }

    pub async fn set_network_mode(&self, mode: NetworkMode) -> Result<()> {
        loop {
            self.reload_if_needed().await?;
            let (candidate, constraints) = {
                let guard = self.state.read().await;
                let mut candidate = guard.config.clone();
                candidate.network.mode = mode;
                (candidate, guard.constraints.clone())
            };

            validate_policy_against_constraints(&candidate, &constraints)
                .map_err(NetworkProxyConstraintError::into_anyhow)
                .context("network.mode constrained by system requirements")?;

            let mut guard = self.state.write().await;
            if guard.constraints != constraints {
                drop(guard);
                continue;
            }
            guard.config.network.mode = mode;
            info!("updated network mode to {mode:?}");
            return Ok(());
        }
    }

    pub async fn mitm_state(&self) -> Result<Option<Arc<MitmState>>> {
        self.reload_if_needed().await?;
        let guard = self.state.read().await;
        Ok(guard.mitm.clone())
    }

    pub async fn add_allowed_domain(&self, host: &str) -> Result<()> {
        self.update_domain_list(host, DomainListKind::Allow).await
    }

    pub async fn add_denied_domain(&self, host: &str) -> Result<()> {
        self.update_domain_list(host, DomainListKind::Deny).await
    }

    async fn update_domain_list(&self, host: &str, target: DomainListKind) -> Result<()> {
        let host = Host::parse(host).context("invalid network host")?;
        let normalized_host = host.as_str().to_string();
        let list_name = target.list_name();
        let constraint_field = target.constraint_field();

        loop {
            self.reload_if_needed().await?;
            let (previous_cfg, constraints, blocked, blocked_total) = {
                let guard = self.state.read().await;
                (
                    guard.config.clone(),
                    guard.constraints.clone(),
                    guard.blocked.clone(),
                    guard.blocked_total,
                )
            };

            let mut candidate = previous_cfg.clone();
            let (target_entries, opposite_entries) = candidate.split_domain_lists_mut(target);
            let target_contains = target_entries
                .iter()
                .any(|entry| normalize_host(entry) == normalized_host);
            let opposite_contains = opposite_entries
                .iter()
                .any(|entry| normalize_host(entry) == normalized_host);
            if target_contains && !opposite_contains {
                return Ok(());
            }

            target_entries.retain(|entry| normalize_host(entry) != normalized_host);
            target_entries.push(normalized_host.clone());
            opposite_entries.retain(|entry| normalize_host(entry) != normalized_host);

            validate_policy_against_constraints(&candidate, &constraints)
                .map_err(NetworkProxyConstraintError::into_anyhow)
                .with_context(|| {
                    format!("{constraint_field} constrained by system requirements")
                })?;

            let mut new_state = build_config_state(candidate.clone(), constraints.clone())
                .with_context(|| format!("failed to compile updated network {list_name}"))?;
            new_state.blocked = blocked;
            new_state.blocked_total = blocked_total;

            let mut guard = self.state.write().await;
            if guard.constraints != constraints || guard.config != previous_cfg {
                drop(guard);
                continue;
            }

            log_policy_changes(&guard.config, &candidate);
            *guard = new_state;
            info!("updated network {list_name} with {normalized_host}");
            return Ok(());
        }
    }

    pub(super) async fn reload_if_needed(&self) -> Result<()> {
        match self.reloader.maybe_reload().await? {
            None => Ok(()),
            Some(mut new_state) => {
                let (previous_cfg, blocked, blocked_total) = {
                    let guard = self.state.read().await;
                    (
                        guard.config.clone(),
                        guard.blocked.clone(),
                        guard.blocked_total,
                    )
                };
                log_policy_changes(&previous_cfg, &new_state.config);
                new_state.blocked = blocked;
                new_state.blocked_total = blocked_total;
                {
                    let mut guard = self.state.write().await;
                    *guard = new_state;
                }
                let source = self.reloader.source_label();
                info!("reloaded config from {source}");
                Ok(())
            }
        }
    }
}
