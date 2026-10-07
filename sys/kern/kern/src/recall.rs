//! PostgreSQL-gated, process-shared recall, prepared only on first enable.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use chaos_recall::{RecallConfig, RecallService, RecallState};
use chaos_vfs::Vfs;
use futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;

use crate::chaos::{Session, TurnContext};
use preparation::{Preparation, TIMEOUT};

mod assets;
mod preparation;
mod preview;
pub(crate) use preview::inject;

type Initialized = Result<Arc<RecallService>, String>;
type Loader = Arc<
    dyn Fn(assets::DownloadRoute, CancellationToken) -> BoxFuture<'static, Initialized>
        + Send
        + Sync,
>;

static RUNTIME: OnceLock<Arc<RecallRuntime>> = OnceLock::new();

pub(crate) struct RecallRuntime {
    preparation: Preparation<Arc<RecallService>>,
    loader: Loader,
}

impl RecallRuntime {
    fn new(loader: Loader) -> Self {
        Self {
            preparation: Preparation::new(TIMEOUT),
            loader,
        }
    }

    pub(crate) fn ready_service(&self) -> Option<Arc<RecallService>> {
        self.preparation.ready()
    }

    async fn prepare(&self, route: assets::DownloadRoute) -> Initialized {
        let loader = self.loader.clone();
        self.preparation
            .prepare(move |cancellation| loader(route, cancellation))
            .await
    }
}

/// Session startup does not touch artifacts or load the model.
/// The process has one configured VFS and CHAOS_HOME.
pub(crate) fn for_session(chaos_home: PathBuf) -> Option<Arc<RecallRuntime>> {
    for_mount(chaos_vfs::pool().ok(), chaos_home, &RUNTIME)
}

fn for_mount(
    mount: Option<Vfs>,
    chaos_home: PathBuf,
    cache: &OnceLock<Arc<RecallRuntime>>,
) -> Option<Arc<RecallRuntime>> {
    let Some(mount @ Vfs::Postgres(_)) = mount else {
        return None;
    };
    Some(
        cache
            .get_or_init(|| Arc::new(runtime_for_mount(mount, chaos_home)))
            .clone(),
    )
}

fn runtime_for_mount(mount: Vfs, chaos_home: PathBuf) -> RecallRuntime {
    RecallRuntime::new(Arc::new(move |route, cancellation| {
        let mount = mount.clone();
        let cache = chaos_home.join("models").join("recall");
        Box::pin(async move {
            let directory = assets::ensure(cache, route, cancellation.clone()).await?;
            if cancellation.is_cancelled() {
                return Err("Recall preparation cancelled".into());
            }
            tracing::info!("recall loading local model and binding PostgreSQL index");
            match RecallState::from_mount(Some(mount), RecallConfig::new(directory), cancellation)
                .await
                .map_err(|error| error.to_string())?
            {
                RecallState::Ready(service) => Ok(Arc::new(service)),
                RecallState::Disabled => Err("Recall requires a PostgreSQL VFS mount".into()),
            }
        })
    }))
}

pub(crate) async fn prepare(session: &Session, turn: &TurnContext) -> Result<(), String> {
    let services = &session.services;
    let config = &turn.config;
    let runtime = services
        .recall
        .as_ref()
        .ok_or_else(|| "Recall requires a PostgreSQL VFS mount with pgvector".to_owned())?;
    // Config is the base policy. The actor includes live updates and approved
    // turn/session grants, just as it does for shell and MCP requests.
    let socket_policy = session
        .permission_snapshot(turn)
        .await
        .effective_socket_policy();
    let route = if !socket_policy.is_enabled() {
        Err("network sockets are disabled".into())
    } else if config.permissions.network.is_some() || services.network_proxy.is_some() {
        Err("recall downloads do not yet support managed network proxies".into())
    } else {
        config
            .egress_url
            .as_deref()
            .map(chaos_client::Egress::parse)
            .transpose()
    };
    runtime.prepare(route).await.map(|_| ()).map_err(|error| {
        format!(
            "Recall activation failed: {error}. Check local artifacts and PostgreSQL/pgvector \
         permissions, then retry enable_tools. Model changes require explicit reindexing."
        )
    })
}

/// Initially enabled clamp groups activate at their first router build. A
/// failure disables only recall, preserving the session and explicit retry.
pub(crate) async fn prepare_for_router(session: &Session, turn: &TurnContext) -> Option<String> {
    let services = &session.services;
    if !services
        .tool_group_catalog
        .is_group_enabled(&services.tool_group_state, crate::tools::groups::RECALL)
    {
        return None;
    }
    let error = prepare(session, turn).await.err()?;
    let _ = services.tool_group_catalog.set_groups_enabled(
        &services.tool_group_state,
        [crate::tools::groups::RECALL],
        false,
    );
    Some(error)
}

#[cfg(test)]
mod tests;
