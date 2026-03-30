use crate::config::ConfigToml;
use crate::config::edit::ConfigEditsBuilder;
use crate::rollout::list::ProcessSortKey;
use crate::state_db;
use chaos_ipc::config_types::Personality;
use chaos_ipc::protocol::SessionSource;
use chaos_journald::JournalRpcClient;
use std::io;
use std::path::Path;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

pub const PERSONALITY_MIGRATION_FILENAME: &str = ".personality_migration";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonalityMigrationStatus {
    SkippedMarker,
    SkippedExplicitPersonality,
    SkippedNoSessions,
    Applied,
}

pub async fn maybe_migrate_personality(
    codex_home: &Path,
    config_toml: &ConfigToml,
) -> io::Result<PersonalityMigrationStatus> {
    let marker_path = codex_home.join(PERSONALITY_MIGRATION_FILENAME);
    if tokio::fs::try_exists(&marker_path).await? {
        return Ok(PersonalityMigrationStatus::SkippedMarker);
    }

    let config_profile = config_toml
        .get_config_profile(/*override_profile*/ None)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    if config_toml.personality.is_some() || config_profile.personality.is_some() {
        create_marker(&marker_path).await?;
        return Ok(PersonalityMigrationStatus::SkippedExplicitPersonality);
    }

    let model_provider_id = config_profile
        .model_provider
        .or_else(|| config_toml.model_provider.clone())
        .unwrap_or_else(|| "openai".to_string());

    if !has_recorded_sessions(codex_home, model_provider_id.as_str()).await? {
        create_marker(&marker_path).await?;
        return Ok(PersonalityMigrationStatus::SkippedNoSessions);
    }

    ConfigEditsBuilder::new(codex_home)
        .set_personality(Some(Personality::Pragmatic))
        .apply()
        .await
        .map_err(|err| {
            io::Error::other(format!("failed to persist personality migration: {err}"))
        })?;

    create_marker(&marker_path).await?;
    Ok(PersonalityMigrationStatus::Applied)
}

async fn has_recorded_sessions(codex_home: &Path, default_provider: &str) -> io::Result<bool> {
    let allowed_sources: &[SessionSource] = &[];

    if let Some(state_db_ctx) = state_db::open_if_present(codex_home, default_provider).await
        && let Some(ids) = state_db::list_process_ids_db(
            Some(state_db_ctx.as_ref()),
            codex_home,
            /*page_size*/ 1,
            /*cursor*/ None,
            ProcessSortKey::CreatedAt,
            allowed_sources,
            /*model_providers*/ None,
            /*archived_only*/ false,
            "personality_migration",
        )
        .await
        && !ids.is_empty()
    {
        return Ok(true);
    }
    let (client, _paths) = match JournalRpcClient::default_or_bootstrap(None).await {
        Ok(client) => client,
        Err(err) => {
            return Err(io::Error::other(format!(
                "failed to connect to journald while checking recorded sessions: {err}"
            )));
        }
    };
    let _ = (default_provider, allowed_sources, ProcessSortKey::CreatedAt);
    let sessions = client
        .list_processes(Some(false))
        .await
        .map_err(io::Error::other)?;
    if !sessions.is_empty() {
        return Ok(true);
    }
    let archived_sessions = client
        .list_processes(Some(true))
        .await
        .map_err(io::Error::other)?;
    Ok(!archived_sessions.is_empty())
}

async fn create_marker(marker_path: &Path) -> io::Result<()> {
    match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(marker_path)
        .await
    {
        Ok(mut file) => file.write_all(b"v1\n").await,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err),
    }
}
