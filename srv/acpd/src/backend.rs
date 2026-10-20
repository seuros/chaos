use std::path::Path;
use std::sync::Arc;

use acp_host::{AgentBackend, AgentSession, protocol};
use chaos_ipc::protocol::{InitialHistory, ResumedHistory, SessionSource};
use chaos_kern::config::Config;
use chaos_kern::models_manager::CollaborationModesConfig;
use chaos_kern::{AuthManager, ProcessTable};
use protocol::schema::v1::{
    ListSessionsRequest, ListSessionsResponse, LoadSessionRequest, McpServer, NewSessionRequest,
    ResumeSessionRequest, SessionCapabilities, SessionListCapabilities, SessionResumeCapabilities,
};

use crate::{Backend, config, history, internal, invalid, session};

impl Backend {
    async fn checked_config(
        &self,
        cwd: &Path,
        roots: &[std::path::PathBuf],
        mcp: &[McpServer],
    ) -> protocol::Result<Config> {
        if !cwd.is_absolute() || !roots.is_empty() {
            return Err(invalid(
                "ACP requires an absolute cwd and does not support additional roots",
            ));
        }
        let cwd = tokio::fs::canonicalize(cwd)
            .await
            .map_err(|_| invalid("ACP cwd is unavailable"))?;
        if !tokio::fs::metadata(&cwd)
            .await
            .is_ok_and(|entry| entry.is_dir())
        {
            return Err(invalid("ACP cwd must be a directory"));
        }
        let config = self.0.load(&cwd).await?;
        config::validate_mcp(&config, mcp)?;
        Ok(config)
    }

    async fn start(
        &self,
        mut config: Config,
        saved: Option<history::Saved>,
        replay: bool,
    ) -> protocol::Result<Arc<dyn AgentSession>> {
        let auth = AuthManager::shared(
            config.chaos_home.clone(),
            false,
            config.cli_auth_credentials_store_mode,
        );
        let table = Arc::new(ProcessTable::new(
            &config,
            auth.clone(),
            SessionSource::Acp,
            CollaborationModesConfig {
                default_mode_request_user_input: false,
            },
        ));
        let guard = session::OwnerGuard::new(table.clone());
        if let Some(saved) = &saved {
            let models = table
                .list_models(chaos_kern::models_manager::manager::RefreshStrategy::Offline)
                .await;
            if config.model.as_deref() != Some(saved.model.as_str())
                && !models.iter().any(|model| {
                    model.model == saved.model && model.show_in_picker && model.supported_in_api
                })
            {
                return Err(invalid(
                    "saved model is unavailable in the selected provider",
                ));
            }
            config.model = Some(saved.model.clone());
        }
        let started = match &saved {
            Some(saved) => {
                table
                    .resume_process_with_history(
                        config.clone(),
                        InitialHistory::Resumed(ResumedHistory {
                            conversation_id: saved.id,
                            history: saved.items.clone(),
                        }),
                        auth,
                        false,
                        None,
                    )
                    .await
            }
            None => table.start_process(config.clone()).await,
        }
        .map_err(|_| internal("native ACP session startup or journal ownership failed"))?;
        let (id, process, _) = started.into_parts();
        let ready = async {
            if let Some(saved) = &saved {
                process
                    .restore_session_controls(&saved.model, &saved.mode)
                    .await
                    .map_err(|_| {
                        invalid("saved model or mode is not allowed by current native policy")
                    })?;
            }
            process
                .confirm_session_durable()
                .await
                .map_err(|_| internal("native ACP journal commit is unconfirmed"))?;
            history::check_record(&config, id).await?;
            Ok::<_, protocol::Error>(())
        }
        .await;
        if let Err(error) = ready {
            guard.shutdown().await?;
            return Err(error);
        }
        let native = session::NativeSession::new(process, id, guard).with_history(
            saved
                .filter(|_| replay)
                .map(|saved| saved.items)
                .unwrap_or_default(),
        );
        Ok(Arc::new(native))
    }
}

impl AgentBackend for Backend {
    fn session_capabilities(&self) -> SessionCapabilities {
        SessionCapabilities::new()
            .list(SessionListCapabilities::new())
            .resume(SessionResumeCapabilities::new())
    }

    fn load_session_supported(&self) -> bool {
        true
    }

    fn new_session(
        &self,
        request: NewSessionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<Arc<dyn AgentSession>>> {
        Box::pin(async move {
            let config = self
                .checked_config(
                    &request.cwd,
                    &request.additional_directories,
                    &request.mcp_servers,
                )
                .await?;
            self.start(config, None, false).await
        })
    }

    fn load_session(
        &self,
        request: LoadSessionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<Arc<dyn AgentSession>>> {
        Box::pin(async move {
            let config = self
                .checked_config(
                    &request.cwd,
                    &request.additional_directories,
                    &request.mcp_servers,
                )
                .await?;
            let saved = history::load(&config, &request.session_id).await?;
            self.start(config, Some(saved), true).await
        })
    }

    fn resume_session(
        &self,
        request: ResumeSessionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<Arc<dyn AgentSession>>> {
        Box::pin(async move {
            let config = self
                .checked_config(
                    &request.cwd,
                    &request.additional_directories,
                    &request.mcp_servers,
                )
                .await?;
            let saved = history::load(&config, &request.session_id).await?;
            self.start(config, Some(saved), false).await
        })
    }

    fn list_sessions(
        &self,
        request: ListSessionsRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<ListSessionsResponse>> {
        Box::pin(async move {
            let base = self.0.base();
            let cwd = match (&self.0, request.cwd) {
                (_, Some(cwd)) => Some(self.checked_config(&cwd, &[], &[]).await?.cwd),
                (crate::ConfigSource::Fixed(_), None) => Some(
                    tokio::fs::canonicalize(&base.cwd)
                        .await
                        .map_err(|_| invalid("trusted ACP workspace is unavailable"))?,
                ),
                (_, None) => None,
            };
            history::list(base, cwd.as_deref(), request.cursor).await
        })
    }
}
