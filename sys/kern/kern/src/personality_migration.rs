use crate::config::ConfigToml;
use crate::config::edit::ConfigEditsBuilder;
use crate::rollout::list::ProcessSortKey;
use crate::runtime_db;
use chaos_ipc::config_types::Personality;
use chaos_ipc::protocol::SessionSource;
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
    chaos_home: &Path,
    config_toml: &ConfigToml,
) -> io::Result<PersonalityMigrationStatus> {
    let marker_path = chaos_home.join(PERSONALITY_MIGRATION_FILENAME);
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

    let sqlite_home = config_toml
        .sqlite_home
        .as_ref()
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| chaos_home.to_path_buf());
    if !has_recorded_sessions(
        config_toml.storage_url.as_deref(),
        sqlite_home.as_path(),
        model_provider_id.as_str(),
    )
    .await?
    {
        create_marker(&marker_path).await?;
        return Ok(PersonalityMigrationStatus::SkippedNoSessions);
    }

    ConfigEditsBuilder::new(chaos_home)
        .set_personality(Some(Personality::Pragmatic))
        .apply()
        .await
        .map_err(|err| {
            io::Error::other(format!("failed to persist personality migration: {err}"))
        })?;

    create_marker(&marker_path).await?;
    Ok(PersonalityMigrationStatus::Applied)
}

async fn has_recorded_sessions(
    storage_url: Option<&str>,
    sqlite_home: &Path,
    default_provider: &str,
) -> io::Result<bool> {
    let allowed_sources: &[SessionSource] = &[];
    let runtime_db_ctx = runtime_db::open_or_create_runtime_db_with_config(
        storage_url,
        sqlite_home,
        default_provider,
    )
    .await
    .map_err(|err| io::Error::other(format!("failed to open runtime storage: {err}")))?;

    for archived_only in [false, true] {
        if let Some(ids) = runtime_db::list_process_ids_db(
            Some(&runtime_db_ctx),
            sqlite_home,
            /*page_size*/ 1,
            /*cursor*/ None,
            ProcessSortKey::CreatedAt,
            allowed_sources,
            /*model_providers*/ None,
            archived_only,
            "personality_migration",
        )
        .await
            && !ids.is_empty()
        {
            return Ok(true);
        }
    }
    Ok(false)
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
