//! MCP registry and per-server actors.

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use anyhow::Context;
use arc_swap::ArcSwap;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_mcp_runtime::PaginatedRequestParams;
use chaos_mcp_runtime::manager::McpConnectionManager;
use chaos_mcp_runtime::manager::SandboxState;
use chaos_sysctl::Constrained;
use chaos_sysctl::types::McpServerConfig;
use chaos_traits::catalog::CatalogTool;
use chaos_traits::router::Adapter;
use chaos_traits::router::AdapterError;
use chaos_traits::router::DEFAULT_ADAPTER_CAPACITY;
use futures::future::join_all;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::catalog::McpCatalogGate;

type ErasedResult = anyhow::Result<Box<dyn Any + Send>>;
type ServerFuture = Pin<Box<dyn Future<Output = ErasedResult> + Send>>;
type ServerJob =
    Box<dyn FnOnce(Arc<McpConnectionManager>, String) -> ServerFuture + Send + 'static>;
type RefreshFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

#[derive(Clone)]
struct McpPermissionState {
    approval_policy: Constrained<ApprovalPolicy>,
    sandbox_state: SandboxState,
}

enum McpServerOp {
    Execute {
        job: ServerJob,
        reply: oneshot::Sender<ErasedResult>,
    },
    ApplySandboxState {
        sandbox_state: SandboxState,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
struct McpServerActor {
    mailbox: Adapter<McpServerOp>,
}

impl McpServerActor {
    fn spawn(name: String, manager: Arc<McpConnectionManager>) -> Self {
        let (mailbox, mut receiver) = Adapter::bounded(DEFAULT_ADAPTER_CAPACITY);
        tokio::spawn(async move {
            while let Some(packet) = receiver.recv().await {
                match packet.op {
                    McpServerOp::Execute { job, reply } => {
                        let result = job(Arc::clone(&manager), name.clone()).await;
                        let _ = reply.send(result);
                    }
                    McpServerOp::ApplySandboxState { sandbox_state } => {
                        if let Err(err) = manager
                            .notify_server_sandbox_state_change(&name, &sandbox_state)
                            .await
                        {
                            warn!(
                                "failed to apply queued MCP sandbox state for server {name}: {err:#}"
                            );
                        }
                    }
                    McpServerOp::Shutdown { reply } => {
                        let _ = reply.send(());
                        break;
                    }
                }
            }
        });
        Self { mailbox }
    }

    async fn execute(
        &self,
        job: ServerJob,
        reply: oneshot::Sender<ErasedResult>,
    ) -> Result<(), AdapterError> {
        self.mailbox.send(McpServerOp::Execute { job, reply }).await
    }

    async fn apply_sandbox_state(&self, sandbox_state: SandboxState) -> Result<(), AdapterError> {
        self.mailbox
            .send(McpServerOp::ApplySandboxState { sandbox_state })
            .await
    }

    async fn shutdown(&self) -> Result<(), AdapterError> {
        let (reply, response) = oneshot::channel();
        self.mailbox.send(McpServerOp::Shutdown { reply }).await?;
        response.await.map_err(|_| AdapterError::ReplyDropped)
    }
}

enum McpRegistryCommand {
    Dispatch {
        server: String,
        job: ServerJob,
        reply: oneshot::Sender<ErasedResult>,
    },
    Install {
        manager: Arc<McpConnectionManager>,
        configs: HashMap<String, McpServerConfig>,
        cancellation_token: CancellationToken,
        catalog_gate: Arc<McpCatalogGate>,
        catalog_tools: Vec<(String, CatalogTool)>,
        bump_revision: bool,
        reply: oneshot::Sender<McpRegistryDiff>,
    },
    SyncPermissionState {
        state: McpPermissionState,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    #[cfg(test)]
    CancellationToken {
        reply: oneshot::Sender<CancellationToken>,
    },
    Cancel {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct McpRegistryDiff {
    pub(crate) revision: u64,
    pub(crate) added: Vec<String>,
    pub(crate) updated: Vec<String>,
    pub(crate) removed: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct McpRegistryActor {
    mailbox: Adapter<McpRegistryCommand>,
    current: Arc<ArcSwap<McpConnectionManager>>,
    configs: Arc<ArcSwap<HashMap<String, McpServerConfig>>>,
    revision: Arc<AtomicU64>,
}

impl McpRegistryActor {
    pub(crate) fn spawn(
        initial_manager: McpConnectionManager,
        initial_cancellation_token: CancellationToken,
    ) -> Self {
        let current = Arc::new(ArcSwap::from_pointee(initial_manager));
        let configs = Arc::new(ArcSwap::from_pointee(HashMap::new()));
        let revision = Arc::new(AtomicU64::new(0));
        let (mailbox, mut receiver) = Adapter::bounded(DEFAULT_ADAPTER_CAPACITY);

        let actor_current = Arc::clone(&current);
        let actor_configs = Arc::clone(&configs);
        let actor_revision = Arc::clone(&revision);
        tokio::spawn(async move {
            let mut server_actors = HashMap::<String, McpServerActor>::new();
            let mut cancellation_token = initial_cancellation_token;
            let mut catalog_gate: Option<Arc<McpCatalogGate>> = None;
            let mut permission_state: Option<McpPermissionState> = None;

            while let Some(packet) = receiver.recv().await {
                match packet.op {
                    McpRegistryCommand::Dispatch { server, job, reply } => {
                        match server_actors.get(&server) {
                            Some(actor) => {
                                actor.execute(job, reply).await.unwrap_or_else(|_| {
                                    panic!("MCP server actor stopped during dispatch");
                                });
                            }
                            None => {
                                let _ = reply.send(Err(anyhow::anyhow!(
                                    "unknown or disabled MCP server `{server}`"
                                )));
                            }
                        }
                    }
                    McpRegistryCommand::Install {
                        manager,
                        configs: next_configs,
                        cancellation_token: next_cancellation_token,
                        catalog_gate: next_catalog_gate,
                        catalog_tools,
                        bump_revision,
                        reply,
                    } => {
                        apply_permission_state_to_manager(&manager, permission_state.as_ref());
                        let (mut diff, retired_actors) = reconcile_registry(
                            &mut server_actors,
                            actor_configs.load().as_ref(),
                            Arc::clone(&manager),
                            &next_configs,
                        );
                        if let Some(state) = permission_state.as_ref() {
                            queue_sandbox_state(&server_actors, state).await;
                        }
                        if let Some(previous_catalog_gate) = catalog_gate.take() {
                            previous_catalog_gate.retire();
                        }
                        next_catalog_gate.activate(catalog_tools);
                        catalog_gate = Some(next_catalog_gate);
                        actor_current.store(manager);
                        actor_configs.store(Arc::new(next_configs));
                        let retired_cancellation_token =
                            std::mem::replace(&mut cancellation_token, next_cancellation_token);
                        drain_generation(retired_actors, retired_cancellation_token);
                        diff.revision = if bump_revision {
                            actor_revision
                                .fetch_add(1, Ordering::SeqCst)
                                .checked_add(1)
                                .unwrap_or_else(|| panic!("MCP registry revision overflow"))
                        } else {
                            actor_revision.load(Ordering::SeqCst)
                        };
                        let _ = reply.send(diff);
                    }
                    McpRegistryCommand::SyncPermissionState { state, reply } => {
                        permission_state = Some(state.clone());
                        let result = synchronize_permission_state(
                            &server_actors,
                            actor_current.load_full(),
                            &state,
                        )
                        .await;
                        let _ = reply.send(result);
                    }
                    #[cfg(test)]
                    McpRegistryCommand::CancellationToken { reply } => {
                        let _ = reply.send(cancellation_token.clone());
                    }
                    McpRegistryCommand::Cancel { reply } => {
                        cancellation_token.cancel();
                        let _ = reply.send(());
                    }
                }
            }

            cancellation_token.cancel();
        });

        Self {
            mailbox,
            current,
            configs,
            revision,
        }
    }

    pub(crate) fn current_manager(&self) -> Arc<McpConnectionManager> {
        self.current.load_full()
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }

    pub(crate) fn configs_snapshot(&self) -> Arc<HashMap<String, McpServerConfig>> {
        self.configs.load_full()
    }

    pub(crate) fn has_servers(&self) -> bool {
        self.configs.load().values().any(|config| config.enabled)
    }

    pub(crate) fn server_origin(&self, server: &str) -> Option<String> {
        self.current_manager()
            .server_origin(server)
            .map(str::to_owned)
    }

    pub(crate) async fn bootstrap(
        &self,
        manager: McpConnectionManager,
        configs: HashMap<String, McpServerConfig>,
        cancellation_token: CancellationToken,
        catalog_gate: Arc<McpCatalogGate>,
        catalog_tools: Vec<(String, CatalogTool)>,
    ) -> anyhow::Result<McpRegistryDiff> {
        self.install(
            manager,
            configs,
            cancellation_token,
            catalog_gate,
            catalog_tools,
            false,
        )
        .await
    }

    pub(crate) async fn reconcile(
        &self,
        manager: McpConnectionManager,
        configs: HashMap<String, McpServerConfig>,
        cancellation_token: CancellationToken,
        catalog_gate: Arc<McpCatalogGate>,
        catalog_tools: Vec<(String, CatalogTool)>,
    ) -> anyhow::Result<McpRegistryDiff> {
        self.install(
            manager,
            configs,
            cancellation_token,
            catalog_gate,
            catalog_tools,
            true,
        )
        .await
    }

    async fn install(
        &self,
        manager: McpConnectionManager,
        configs: HashMap<String, McpServerConfig>,
        cancellation_token: CancellationToken,
        catalog_gate: Arc<McpCatalogGate>,
        catalog_tools: Vec<(String, CatalogTool)>,
        bump_revision: bool,
    ) -> anyhow::Result<McpRegistryDiff> {
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(McpRegistryCommand::Install {
                manager: Arc::new(manager),
                configs,
                cancellation_token,
                catalog_gate,
                catalog_tools,
                bump_revision,
                reply,
            })
            .await
            .context("MCP registry actor is unavailable")?;
        response
            .await
            .context("MCP registry actor dropped the install reply")
    }

    #[cfg(test)]
    pub(crate) async fn cancellation_token(&self) -> CancellationToken {
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(McpRegistryCommand::CancellationToken { reply })
            .await
            .expect("MCP registry actor stopped in test");
        response
            .await
            .expect("MCP registry actor dropped the cancellation token reply")
    }

    pub(crate) async fn cancel(&self) -> anyhow::Result<()> {
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(McpRegistryCommand::Cancel { reply })
            .await
            .context("MCP registry actor is unavailable")?;
        response
            .await
            .context("MCP registry actor dropped the cancellation reply")
    }

    pub(crate) async fn sync_permission_state(
        &self,
        approval_policy: Constrained<ApprovalPolicy>,
        sandbox_state: SandboxState,
    ) -> anyhow::Result<()> {
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(McpRegistryCommand::SyncPermissionState {
                state: McpPermissionState {
                    approval_policy,
                    sandbox_state,
                },
                reply,
            })
            .await
            .context("MCP registry actor is unavailable")?;
        response
            .await
            .context("MCP registry actor dropped the permission reply")?
    }

    pub(crate) async fn execute<T, F, Fut>(&self, server: &str, job: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<McpConnectionManager>, String) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        let (reply, response) = oneshot::channel();
        let erased_job: ServerJob = Box::new(move |manager, server| {
            Box::pin(async move {
                let value = job(manager, server).await?;
                Ok(Box::new(value) as Box<dyn Any + Send>)
            })
        });
        self.mailbox
            .send(McpRegistryCommand::Dispatch {
                server: server.to_string(),
                job: erased_job,
                reply,
            })
            .await
            .context("MCP registry actor is unavailable")?;
        let erased = response
            .await
            .context("MCP server actor dropped its reply")??;
        erased
            .downcast::<T>()
            .map(|value| *value)
            .map_err(|_| anyhow::anyhow!("MCP server actor returned an unexpected reply type"))
    }

    pub(crate) async fn list_all_resources(
        &self,
    ) -> HashMap<String, Vec<chaos_mcp_runtime::ResourceInfo>> {
        let names = self.server_names();
        let calls = names.into_iter().map(|server| {
            let actor = self.clone();
            async move {
                let result = actor
                    .execute(&server, |manager, server| async move {
                        manager
                            .list_resources(&server, None::<PaginatedRequestParams>)
                            .await
                            .map(|result| result.resources)
                    })
                    .await;
                (server, result)
            }
        });
        collect_successful_servers(join_all(calls).await, "list resources")
    }

    pub(crate) async fn list_all_resource_templates(
        &self,
    ) -> HashMap<String, Vec<chaos_mcp_runtime::ResourceTemplateInfo>> {
        let names = self.server_names();
        let calls = names.into_iter().map(|server| {
            let actor = self.clone();
            async move {
                let result = actor
                    .execute(&server, |manager, server| async move {
                        manager
                            .list_resource_templates(&server, None::<PaginatedRequestParams>)
                            .await
                            .map(|result| result.resource_templates)
                    })
                    .await;
                (server, result)
            }
        });
        collect_successful_servers(join_all(calls).await, "list resource templates")
    }

    pub(crate) async fn notify_roots_changed(&self, new_cwd: &Path) -> anyhow::Result<()> {
        let calls = self.server_names().into_iter().map(|server| {
            let actor = self.clone();
            let new_cwd = new_cwd.to_path_buf();
            async move {
                actor
                    .execute(&server, move |manager, server| async move {
                        manager.notify_server_roots_changed(&server, &new_cwd).await
                    })
                    .await
            }
        });
        collect_broadcast_results(join_all(calls).await, "notify roots changed")
    }

    fn server_names(&self) -> Vec<String> {
        let mut names = self
            .configs
            .load()
            .iter()
            .filter(|(_, config)| config.enabled)
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        names.sort();
        names
    }
}

enum McpRefreshCommand {
    Enqueue {
        job: RefreshFuture,
    },
    Run {
        job: RefreshFuture,
        reply: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
pub(crate) struct McpRefreshActor {
    mailbox: Adapter<McpRefreshCommand>,
}

impl McpRefreshActor {
    pub(crate) fn spawn() -> Self {
        let (mailbox, mut receiver) = Adapter::bounded(DEFAULT_ADAPTER_CAPACITY);
        tokio::spawn(async move {
            while let Some(packet) = receiver.recv().await {
                match packet.op {
                    McpRefreshCommand::Enqueue { job } => job.await,
                    McpRefreshCommand::Run { job, reply } => {
                        job.await;
                        let _ = reply.send(());
                    }
                }
            }
        });
        Self { mailbox }
    }

    pub(crate) async fn enqueue<F>(&self, job: F) -> Result<(), AdapterError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.mailbox
            .send(McpRefreshCommand::Enqueue { job: Box::pin(job) })
            .await
    }

    pub(crate) async fn run<F>(&self, job: F) -> anyhow::Result<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(McpRefreshCommand::Run {
                job: Box::pin(job),
                reply,
            })
            .await
            .context("MCP refresh actor is unavailable")?;
        response
            .await
            .context("MCP refresh actor dropped completion acknowledgement")
    }
}

fn apply_permission_state_to_manager(
    manager: &Arc<McpConnectionManager>,
    permission_state: Option<&McpPermissionState>,
) {
    let Some(state) = permission_state else {
        return;
    };
    manager.set_approval_policy(&state.approval_policy);
}

async fn queue_sandbox_state(actors: &HashMap<String, McpServerActor>, state: &McpPermissionState) {
    for actor in actors.values() {
        actor
            .apply_sandbox_state(state.sandbox_state.clone())
            .await
            .unwrap_or_else(|_| {
                panic!("new MCP server actor stopped during generation cutover");
            });
    }
}

async fn synchronize_permission_state(
    actors: &HashMap<String, McpServerActor>,
    manager: Arc<McpConnectionManager>,
    state: &McpPermissionState,
) -> anyhow::Result<()> {
    manager.set_approval_policy(&state.approval_policy);
    for actor in actors.values() {
        actor
            .apply_sandbox_state(state.sandbox_state.clone())
            .await
            .map_err(adapter_error)?;
    }
    Ok(())
}

fn reconcile_registry(
    actors: &mut HashMap<String, McpServerActor>,
    previous_configs: &HashMap<String, McpServerConfig>,
    manager: Arc<McpConnectionManager>,
    next_configs: &HashMap<String, McpServerConfig>,
) -> (McpRegistryDiff, HashMap<String, McpServerActor>) {
    let previous_enabled = enabled_configs(previous_configs);
    let next_enabled = enabled_configs(next_configs);

    let mut diff = McpRegistryDiff::default();
    for (name, next) in &next_enabled {
        match previous_enabled.get(name) {
            None => diff.added.push((*name).clone()),
            Some(previous) if *previous != *next => diff.updated.push((*name).clone()),
            Some(_) => {}
        }
    }
    for name in previous_enabled.keys() {
        if !next_enabled.contains_key(name) {
            diff.removed.push((*name).clone());
        }
    }
    diff.added.sort();
    diff.updated.sort();
    diff.removed.sort();

    let next_actors = next_enabled
        .keys()
        .map(|name| {
            (
                (*name).clone(),
                McpServerActor::spawn((*name).clone(), Arc::clone(&manager)),
            )
        })
        .collect();
    let retired_actors = std::mem::replace(actors, next_actors);

    (diff, retired_actors)
}

fn enabled_configs(
    configs: &HashMap<String, McpServerConfig>,
) -> HashMap<&String, &McpServerConfig> {
    configs
        .iter()
        .filter(|(_, config)| config.enabled)
        .collect()
}

fn drain_generation(
    actors: HashMap<String, McpServerActor>,
    cancellation_token: CancellationToken,
) {
    tokio::spawn(async move {
        let drains = actors.into_values().map(|actor| async move {
            actor.shutdown().await.unwrap_or_else(|_| {
                panic!("retired MCP server actor stopped before drain");
            });
        });
        join_all(drains).await;
        cancellation_token.cancel();
    });
}

fn collect_successful_servers<T>(
    results: Vec<(String, anyhow::Result<T>)>,
    operation: &str,
) -> HashMap<String, T> {
    let mut collected = HashMap::new();
    for (server, result) in results {
        match result {
            Ok(value) => {
                collected.insert(server, value);
            }
            Err(err) => warn!(server, error = %err, operation, "MCP server operation failed"),
        }
    }
    collected
}

fn collect_broadcast_results(
    results: Vec<anyhow::Result<()>>,
    operation: &str,
) -> anyhow::Result<()> {
    let errors = results
        .into_iter()
        .filter_map(Result::err)
        .map(|err| err.to_string())
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{operation} failed: {}", errors.join("; "))
    }
}

fn adapter_error(error: AdapterError) -> anyhow::Error {
    anyhow::anyhow!("MCP server actor is unavailable: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use crate::catalog::CatalogSink;
    use chaos_ipc::protocol::ApprovalPolicy;
    use chaos_sysctl::Constrained;
    use serde_json::json;
    use tokio::time::Duration;
    use tokio::time::timeout;

    fn manager() -> McpConnectionManager {
        McpConnectionManager::new_uninitialized(&Constrained::allow_any(
            ApprovalPolicy::Interactive,
        ))
    }

    fn gate() -> Arc<McpCatalogGate> {
        Arc::new(McpCatalogGate::staging(Arc::new(CatalogSink::new(
            Catalog::from_inventory(),
        ))))
    }

    #[tokio::test]
    async fn bootstrap_does_not_advance_revision() {
        let actor = McpRegistryActor::spawn(manager(), CancellationToken::new());
        let diff = actor
            .bootstrap(
                manager(),
                HashMap::new(),
                CancellationToken::new(),
                gate(),
                Vec::new(),
            )
            .await
            .expect("bootstrap");
        assert_eq!(diff.revision, 0);
        assert_eq!(actor.revision(), 0);
    }

    #[tokio::test]
    async fn reconcile_advances_revision() {
        let actor = McpRegistryActor::spawn(manager(), CancellationToken::new());
        let diff = actor
            .reconcile(
                manager(),
                HashMap::new(),
                CancellationToken::new(),
                gate(),
                Vec::new(),
            )
            .await
            .expect("reconcile");
        assert_eq!(diff.revision, 1);
        assert_eq!(actor.revision(), 1);
    }

    #[tokio::test]
    async fn disabled_configs_are_not_exposed_as_server_actors() {
        let actor = McpRegistryActor::spawn(manager(), CancellationToken::new());
        let disabled = serde_json::from_value(json!({
            "command": "disabled-server",
            "enabled": false
        }))
        .expect("disabled MCP config");
        actor
            .bootstrap(
                manager(),
                HashMap::from([("disabled".to_string(), disabled)]),
                CancellationToken::new(),
                gate(),
                Vec::new(),
            )
            .await
            .expect("bootstrap");

        assert!(!actor.has_servers());
        assert!(actor.server_names().is_empty());
    }

    #[tokio::test]
    async fn refresh_actor_preserves_order_without_blocking_enqueue() {
        let actor = McpRefreshActor::spawn();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let (order_tx, mut order_rx) = tokio::sync::mpsc::unbounded_channel();

        let first_order_tx = order_tx.clone();
        actor
            .enqueue(async move {
                let _ = started_tx.send(());
                let _ = release_rx.await;
                let _ = first_order_tx.send(1);
            })
            .await
            .expect("enqueue first refresh");
        actor
            .enqueue(async move {
                let _ = order_tx.send(2);
            })
            .await
            .expect("enqueue second refresh");

        started_rx.await.expect("first refresh started");
        assert!(
            timeout(Duration::from_millis(20), order_rx.recv())
                .await
                .is_err(),
            "the second refresh must remain queued while the first is running"
        );
        release_tx.send(()).expect("release first refresh");
        assert_eq!(order_rx.recv().await, Some(1));
        assert_eq!(order_rx.recv().await, Some(2));
    }

    #[tokio::test]
    async fn permission_sync_does_not_wait_for_running_server_call() {
        let actor = McpRegistryActor::spawn(manager(), CancellationToken::new());
        let enabled: McpServerConfig = serde_json::from_value(json!({
            "command": "enabled-server",
            "enabled": true
        }))
        .expect("enabled MCP config");
        actor
            .bootstrap(
                manager(),
                HashMap::from([("enabled".to_string(), enabled)]),
                CancellationToken::new(),
                gate(),
                Vec::new(),
            )
            .await
            .expect("bootstrap");

        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let executing_actor = actor.clone();
        let execution = tokio::spawn(async move {
            executing_actor
                .execute("enabled", move |_, _| async move {
                    let _ = started_tx.send(());
                    let _ = release_rx.await;
                    Ok(())
                })
                .await
        });
        started_rx.await.expect("server call started");

        let sandbox_state = SandboxState {
            vfs_policy: chaos_ipc::permissions::VfsPolicy::default(),
            socket_policy: chaos_ipc::permissions::SocketPolicy::default(),
            alcatraz_macos_exe: None,
            alcatraz_linux_exe: None,
            alcatraz_freebsd_exe: None,
            sandbox_cwd: std::path::PathBuf::from("/"),
        };
        timeout(
            Duration::from_millis(100),
            actor.sync_permission_state(
                Constrained::allow_any(ApprovalPolicy::Headless),
                sandbox_state,
            ),
        )
        .await
        .expect("permission sync should enqueue without waiting")
        .expect("permission sync");

        release_tx.send(()).expect("release server call");
        execution
            .await
            .expect("server call task")
            .expect("server call");
    }

    #[tokio::test]
    async fn reconcile_does_not_wait_for_running_server_call() {
        let actor = McpRegistryActor::spawn(manager(), CancellationToken::new());
        let enabled: McpServerConfig = serde_json::from_value(json!({
            "command": "enabled-server",
            "enabled": true
        }))
        .expect("enabled MCP config");
        actor
            .bootstrap(
                manager(),
                HashMap::from([("enabled".to_string(), enabled.clone())]),
                CancellationToken::new(),
                gate(),
                Vec::new(),
            )
            .await
            .expect("bootstrap");

        let old_manager = actor.current_manager();
        let old_cancellation_token = actor.cancellation_token().await;
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let executing_actor = actor.clone();
        let execution = tokio::spawn(async move {
            executing_actor
                .execute("enabled", move |manager, _| async move {
                    let _ = started_tx.send(manager);
                    let _ = release_rx.await;
                    Ok(())
                })
                .await
        });
        let running_manager = started_rx.await.expect("server call started");
        assert!(Arc::ptr_eq(&old_manager, &running_manager));

        timeout(
            Duration::from_millis(100),
            actor.reconcile(
                manager(),
                HashMap::from([("enabled".to_string(), enabled)]),
                CancellationToken::new(),
                gate(),
                Vec::new(),
            ),
        )
        .await
        .expect("reconcile should not wait for the old server call")
        .expect("reconcile");

        let new_manager = actor.current_manager();
        assert!(!Arc::ptr_eq(&old_manager, &new_manager));
        let expected_manager = Arc::clone(&new_manager);
        let used_new_generation = timeout(
            Duration::from_millis(100),
            actor.execute("enabled", move |manager, _| async move {
                Ok(Arc::ptr_eq(&manager, &expected_manager))
            }),
        )
        .await
        .expect("new generation call should not wait for the old generation")
        .expect("new generation call");
        assert!(used_new_generation);
        assert!(
            !old_cancellation_token.is_cancelled(),
            "old generation must stay alive while its call is running"
        );

        release_tx.send(()).expect("release old server call");
        execution
            .await
            .expect("old server call task")
            .expect("old server call");
        timeout(Duration::from_secs(1), old_cancellation_token.cancelled())
            .await
            .expect("old generation should be cancelled after it drains");
    }
}
