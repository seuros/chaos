use crate::config;
use crate::http_proxy;
use crate::network_policy::NetworkPolicyDecider;
use crate::runtime::BlockedRequestObserver;
use crate::runtime::unix_socket_permissions_supported;
use crate::socks5;
use crate::state::NetworkProxyState;
use anyhow::Context;
use anyhow::Result;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::net::TcpListener as StdTcpListener;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::warn;

#[derive(Debug)]
struct ReservedListeners {
    http: Mutex<Option<StdTcpListener>>,
    socks: Mutex<Option<StdTcpListener>>,
}

impl ReservedListeners {
    fn new(http: StdTcpListener, socks: Option<StdTcpListener>) -> Self {
        Self {
            http: Mutex::new(Some(http)),
            socks: Mutex::new(socks),
        }
    }

    fn take_http(&self) -> Option<StdTcpListener> {
        let mut guard = self
            .http
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.take()
    }

    fn take_socks(&self) -> Option<StdTcpListener> {
        let mut guard = self
            .socks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.take()
    }
}

#[derive(Clone)]
pub struct NetworkProxyBuilder {
    state: Option<Arc<NetworkProxyState>>,
    http_addr: Option<SocketAddr>,
    socks_addr: Option<SocketAddr>,
    managed_by_codex: bool,
    policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
    blocked_request_observer: Option<Arc<dyn BlockedRequestObserver>>,
}

impl Default for NetworkProxyBuilder {
    fn default() -> Self {
        Self {
            state: None,
            http_addr: None,
            socks_addr: None,
            managed_by_codex: true,
            policy_decider: None,
            blocked_request_observer: None,
        }
    }
}

impl NetworkProxyBuilder {
    pub fn state(mut self, state: Arc<NetworkProxyState>) -> Self {
        self.state = Some(state);
        self
    }

    pub fn http_addr(mut self, addr: SocketAddr) -> Self {
        self.http_addr = Some(addr);
        self
    }

    pub fn socks_addr(mut self, addr: SocketAddr) -> Self {
        self.socks_addr = Some(addr);
        self
    }

    pub fn managed_by_codex(mut self, managed_by_codex: bool) -> Self {
        self.managed_by_codex = managed_by_codex;
        self
    }

    pub fn policy_decider<D>(mut self, decider: D) -> Self
    where
        D: NetworkPolicyDecider,
    {
        self.policy_decider = Some(Arc::new(decider));
        self
    }

    pub fn policy_decider_arc(mut self, decider: Arc<dyn NetworkPolicyDecider>) -> Self {
        self.policy_decider = Some(decider);
        self
    }

    pub fn blocked_request_observer<O>(mut self, observer: O) -> Self
    where
        O: BlockedRequestObserver,
    {
        self.blocked_request_observer = Some(Arc::new(observer));
        self
    }

    pub fn blocked_request_observer_arc(
        mut self,
        observer: Arc<dyn BlockedRequestObserver>,
    ) -> Self {
        self.blocked_request_observer = Some(observer);
        self
    }

    pub async fn build(self) -> Result<NetworkProxy> {
        let state = self.state.ok_or_else(|| {
            anyhow::anyhow!(
                "NetworkProxyBuilder requires a state; supply one via builder.state(...)"
            )
        })?;
        state
            .set_blocked_request_observer(self.blocked_request_observer.clone())
            .await;
        let current_cfg = state.current_cfg().await?;
        let (requested_http_addr, requested_socks_addr, reserved_listeners) =
            if self.managed_by_codex {
                let runtime = config::resolve_runtime(&current_cfg)?;
                let (http_listener, socks_listener) =
                    reserve_loopback_ephemeral_listeners(current_cfg.network.enable_socks5)
                        .context("reserve managed loopback proxy listeners")?;
                let http_addr = http_listener
                    .local_addr()
                    .context("failed to read reserved HTTP proxy address")?;
                let socks_addr = if let Some(socks_listener) = socks_listener.as_ref() {
                    socks_listener
                        .local_addr()
                        .context("failed to read reserved SOCKS5 proxy address")?
                } else {
                    runtime.socks_addr
                };
                (
                    http_addr,
                    socks_addr,
                    Some(Arc::new(ReservedListeners::new(
                        http_listener,
                        socks_listener,
                    ))),
                )
            } else {
                let runtime = config::resolve_runtime(&current_cfg)?;
                (
                    self.http_addr.unwrap_or(runtime.http_addr),
                    self.socks_addr.unwrap_or(runtime.socks_addr),
                    None,
                )
            };

        // Reapply bind clamping for caller overrides so unix-socket proxying stays loopback-only.
        let (http_addr, socks_addr) = config::clamp_bind_addrs(
            requested_http_addr,
            requested_socks_addr,
            &current_cfg.network,
        );

        Ok(NetworkProxy {
            state,
            http_addr,
            socks_addr,
            socks_enabled: current_cfg.network.enable_socks5,
            allow_local_binding: current_cfg.network.allow_local_binding,
            allow_unix_sockets: current_cfg.network.allow_unix_sockets.clone(),
            dangerously_allow_all_unix_sockets: current_cfg
                .network
                .dangerously_allow_all_unix_sockets,
            reserved_listeners,
            policy_decider: self.policy_decider,
        })
    }
}

fn reserve_loopback_ephemeral_listeners(
    reserve_socks_listener: bool,
) -> Result<(StdTcpListener, Option<StdTcpListener>)> {
    let http_listener =
        reserve_loopback_ephemeral_listener().context("reserve HTTP proxy listener")?;
    let socks_listener = if reserve_socks_listener {
        Some(reserve_loopback_ephemeral_listener().context("reserve SOCKS5 proxy listener")?)
    } else {
        None
    };
    Ok((http_listener, socks_listener))
}

fn reserve_loopback_ephemeral_listener() -> Result<StdTcpListener> {
    StdTcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .context("bind loopback ephemeral port")
}

#[derive(Clone)]
pub struct NetworkProxy {
    state: Arc<NetworkProxyState>,
    http_addr: SocketAddr,
    socks_addr: SocketAddr,
    socks_enabled: bool,
    allow_local_binding: bool,
    allow_unix_sockets: Vec<String>,
    dangerously_allow_all_unix_sockets: bool,
    reserved_listeners: Option<Arc<ReservedListeners>>,
    policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
}

impl std::fmt::Debug for NetworkProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Avoid logging internal state (config contents, derived globsets, etc.) which can be noisy
        // and may contain sensitive paths.
        f.debug_struct("NetworkProxy")
            .field("http_addr", &self.http_addr)
            .field("socks_addr", &self.socks_addr)
            .finish_non_exhaustive()
    }
}

impl PartialEq for NetworkProxy {
    fn eq(&self, other: &Self) -> bool {
        self.http_addr == other.http_addr
            && self.socks_addr == other.socks_addr
            && self.allow_local_binding == other.allow_local_binding
    }
}

impl Eq for NetworkProxy {}

pub const PROXY_URL_ENV_KEYS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "WS_PROXY",
    "WSS_PROXY",
    "ALL_PROXY",
    "FTP_PROXY",
    "YARN_HTTP_PROXY",
    "YARN_HTTPS_PROXY",
    "NPM_CONFIG_HTTP_PROXY",
    "NPM_CONFIG_HTTPS_PROXY",
    "NPM_CONFIG_PROXY",
    "BUNDLE_HTTP_PROXY",
    "BUNDLE_HTTPS_PROXY",
    "PIP_PROXY",
    "DOCKER_HTTP_PROXY",
    "DOCKER_HTTPS_PROXY",
];

pub const ALL_PROXY_ENV_KEYS: &[&str] = &["ALL_PROXY", "all_proxy"];
pub const ALLOW_LOCAL_BINDING_ENV_KEY: &str = "CHAOS_NETWORK_ALLOW_LOCAL_BINDING";

const FTP_PROXY_ENV_KEYS: &[&str] = &["FTP_PROXY", "ftp_proxy"];
const WEBSOCKET_PROXY_ENV_KEYS: &[&str] = &["WS_PROXY", "WSS_PROXY", "ws_proxy", "wss_proxy"];

pub const NO_PROXY_ENV_KEYS: &[&str] = &[
    "NO_PROXY",
    "no_proxy",
    "npm_config_noproxy",
    "NPM_CONFIG_NOPROXY",
    "YARN_NO_PROXY",
    "BUNDLE_NO_PROXY",
];

pub const DEFAULT_NO_PROXY_VALUE: &str = concat!(
    "localhost,127.0.0.1,::1,",
    "*.local,.local,",
    "169.254.0.0/16,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16"
);

pub fn proxy_url_env_value<'a>(
    env: &'a HashMap<String, String>,
    canonical_key: &str,
) -> Option<&'a str> {
    if let Some(value) = env.get(canonical_key) {
        return Some(value.as_str());
    }
    let lower_key = canonical_key.to_ascii_lowercase();
    env.get(lower_key.as_str()).map(String::as_str)
}

pub fn has_proxy_url_env_vars(env: &HashMap<String, String>) -> bool {
    PROXY_URL_ENV_KEYS
        .iter()
        .any(|key| proxy_url_env_value(env, key).is_some_and(|value| !value.trim().is_empty()))
}

fn set_env_keys(env: &mut HashMap<String, String>, keys: &[&str], value: &str) {
    for key in keys {
        env.insert((*key).to_string(), value.to_string());
    }
}

fn apply_proxy_env_overrides(
    env: &mut HashMap<String, String>,
    http_addr: SocketAddr,
    socks_addr: SocketAddr,
    socks_enabled: bool,
    allow_local_binding: bool,
) {
    let http_proxy_url = format!("http://{http_addr}");
    let socks_proxy_url = format!("socks5h://{socks_addr}");
    env.insert(
        ALLOW_LOCAL_BINDING_ENV_KEY.to_string(),
        if allow_local_binding {
            "1".to_string()
        } else {
            "0".to_string()
        },
    );

    // HTTP-based clients are best served by explicit HTTP proxy URLs.
    set_env_keys(
        env,
        &[
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "http_proxy",
            "https_proxy",
            "YARN_HTTP_PROXY",
            "YARN_HTTPS_PROXY",
            "npm_config_http_proxy",
            "npm_config_https_proxy",
            "npm_config_proxy",
            "NPM_CONFIG_HTTP_PROXY",
            "NPM_CONFIG_HTTPS_PROXY",
            "NPM_CONFIG_PROXY",
            "BUNDLE_HTTP_PROXY",
            "BUNDLE_HTTPS_PROXY",
            "PIP_PROXY",
            "DOCKER_HTTP_PROXY",
            "DOCKER_HTTPS_PROXY",
        ],
        &http_proxy_url,
    );
    // Some websocket clients look for dedicated WS/WSS proxy environment variables instead of
    // HTTP(S)_PROXY. Keep them aligned with the managed HTTP proxy endpoint.
    set_env_keys(env, WEBSOCKET_PROXY_ENV_KEYS, &http_proxy_url);

    // Keep local/private targets direct so local IPC and metadata endpoints avoid the proxy.
    set_env_keys(env, NO_PROXY_ENV_KEYS, DEFAULT_NO_PROXY_VALUE);

    env.insert("ELECTRON_GET_USE_PROXY".to_string(), "true".to_string());

    // Keep HTTP_PROXY/HTTPS_PROXY as HTTP endpoints. A lot of clients break if
    // those vars contain SOCKS URLs. We only switch ALL_PROXY here.
    //
    if socks_enabled {
        set_env_keys(env, ALL_PROXY_ENV_KEYS, &socks_proxy_url);
        set_env_keys(env, FTP_PROXY_ENV_KEYS, &socks_proxy_url);
    } else {
        set_env_keys(env, ALL_PROXY_ENV_KEYS, &http_proxy_url);
        set_env_keys(env, FTP_PROXY_ENV_KEYS, &http_proxy_url);
    }

    #[cfg(target_os = "macos")]
    if socks_enabled {
        // Preserve existing SSH wrappers (for example: Secretive/Teleport setups)
        // and only provide a SOCKS ProxyCommand fallback when one is not present.
        env.entry("GIT_SSH_COMMAND".to_string())
            .or_insert_with(|| format!("ssh -o ProxyCommand='nc -X 5 -x {socks_addr} %h %p'"));
    }
}

impl NetworkProxy {
    pub fn builder() -> NetworkProxyBuilder {
        NetworkProxyBuilder::default()
    }

    pub fn http_addr(&self) -> SocketAddr {
        self.http_addr
    }

    pub fn socks_addr(&self) -> SocketAddr {
        self.socks_addr
    }

    pub async fn current_cfg(&self) -> Result<config::NetworkProxyConfig> {
        self.state.current_cfg().await
    }

    pub async fn add_allowed_domain(&self, host: &str) -> Result<()> {
        self.state.add_allowed_domain(host).await
    }

    pub async fn add_denied_domain(&self, host: &str) -> Result<()> {
        self.state.add_denied_domain(host).await
    }

    pub fn allow_local_binding(&self) -> bool {
        self.allow_local_binding
    }

    pub fn allow_unix_sockets(&self) -> &[String] {
        &self.allow_unix_sockets
    }

    pub fn dangerously_allow_all_unix_sockets(&self) -> bool {
        self.dangerously_allow_all_unix_sockets
    }

    pub fn apply_to_env(&self, env: &mut HashMap<String, String>) {
        // Enforce proxying for child processes. We intentionally override existing values so
        // command-level environment cannot bypass the managed proxy endpoint.
        apply_proxy_env_overrides(
            env,
            self.http_addr,
            self.socks_addr,
            self.socks_enabled,
            self.allow_local_binding,
        );
    }

    pub async fn run(&self) -> Result<NetworkProxyHandle> {
        let current_cfg = self.state.current_cfg().await?;
        if !current_cfg.network.enabled {
            warn!("network.enabled is false; skipping proxy listeners");
            return Ok(NetworkProxyHandle::noop());
        }

        if !unix_socket_permissions_supported() {
            warn!(
                "allowUnixSockets and dangerouslyAllowAllUnixSockets are macOS-only; requests will be rejected on this platform"
            );
        }

        let reserved_listeners = self.reserved_listeners.as_ref();
        let http_listener = reserved_listeners.and_then(|listeners| listeners.take_http());
        let socks_listener = reserved_listeners.and_then(|listeners| listeners.take_socks());

        let http_state = self.state.clone();
        let http_decider = self.policy_decider.clone();
        let http_addr = self.http_addr;
        let http_task = tokio::spawn(async move {
            match http_listener {
                Some(listener) => {
                    http_proxy::run_http_proxy_with_std_listener(http_state, listener, http_decider)
                        .await
                }
                None => http_proxy::run_http_proxy(http_state, http_addr, http_decider).await,
            }
        });

        let socks_task = if current_cfg.network.enable_socks5 {
            let socks_state = self.state.clone();
            let socks_decider = self.policy_decider.clone();
            let socks_addr = self.socks_addr;
            let enable_socks5_udp = current_cfg.network.enable_socks5_udp;
            Some(tokio::spawn(async move {
                match socks_listener {
                    Some(listener) => {
                        socks5::run_socks5_with_std_listener(
                            socks_state,
                            listener,
                            socks_decider,
                            enable_socks5_udp,
                        )
                        .await
                    }
                    None => {
                        socks5::run_socks5(
                            socks_state,
                            socks_addr,
                            socks_decider,
                            enable_socks5_udp,
                        )
                        .await
                    }
                }
            }))
        } else {
            None
        };

        Ok(NetworkProxyHandle {
            http_task: Some(http_task),
            socks_task,
            completed: false,
        })
    }
}

pub struct NetworkProxyHandle {
    http_task: Option<JoinHandle<Result<()>>>,
    socks_task: Option<JoinHandle<Result<()>>>,
    completed: bool,
}

impl NetworkProxyHandle {
    fn noop() -> Self {
        Self {
            http_task: Some(tokio::spawn(async { Ok(()) })),
            socks_task: None,
            completed: true,
        }
    }

    pub async fn wait(mut self) -> Result<()> {
        let http_task = self.http_task.take().context("missing http proxy task")?;
        let socks_task = self.socks_task.take();
        let http_result = http_task.await;
        let socks_result = match socks_task {
            Some(task) => Some(task.await),
            None => None,
        };
        self.completed = true;
        http_result??;
        if let Some(socks_result) = socks_result {
            socks_result??;
        }
        Ok(())
    }

    pub async fn shutdown(mut self) -> Result<()> {
        abort_tasks(self.http_task.take(), self.socks_task.take()).await;
        self.completed = true;
        Ok(())
    }
}

async fn abort_task(task: Option<JoinHandle<Result<()>>>) {
    if let Some(task) = task {
        task.abort();
        let _ = task.await;
    }
}

async fn abort_tasks(
    http_task: Option<JoinHandle<Result<()>>>,
    socks_task: Option<JoinHandle<Result<()>>>,
) {
    abort_task(http_task).await;
    abort_task(socks_task).await;
}

impl Drop for NetworkProxyHandle {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let http_task = self.http_task.take();
        let socks_task = self.socks_task.take();
        tokio::spawn(async move {
            abort_tasks(http_task, socks_task).await;
        });
    }
}

#[cfg(test)]
mod tests;
