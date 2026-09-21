//! First-run storage setup, before any database-backed configuration is loaded.
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, ensure};

use super::{lock_bootstrap, read_toml, resolve_reference, write_bootstrap};

pub enum StorageChoice {
    Postgres(String),
    Sqlite,
}

/// Do not redirect an existing installation or override unattended provisioning.
/// The installation ID is deliberately not an onboarding-completion marker.
pub fn is_needed(home: &Path) -> anyhow::Result<bool> {
    is_needed_with_environment(
        home,
        std::env::var_os("CHAOS_STORAGE_URL").is_some()
            || chaos_proc::sqlite_home_env_value().is_some(),
    )
}

fn is_needed_with_environment(home: &Path, storage_in_environment: bool) -> anyhow::Result<bool> {
    if storage_in_environment {
        return Ok(false);
    }
    let file = read_toml(home)?;
    if file.get("storage_url").is_some() {
        return Ok(false);
    }
    Ok(!home.join("chaos.sqlite").try_exists()?)
}

fn postgres_url(connection: &str) -> anyhow::Result<String> {
    let resolved = resolve_reference(connection)
        .map_err(|_| anyhow!("Cannot read the connection reference. Check the environment or secure credential store."))?;
    let resolved = resolved.trim();
    let url = url::Url::parse(resolved)
        .map_err(|_| anyhow!("Enter a PostgreSQL URL or an env:VARIABLE reference."))?;
    ensure!(
        matches!(url.scheme(), "postgres" | "postgresql"),
        "Use a postgres:// or postgresql:// connection URL."
    );
    ensure!(
        url.host_str().is_some() && !url.path().trim_matches('/').is_empty(),
        "Include the PostgreSQL host and database name."
    );
    crate::config::normalize_storage_url(Some(resolved))
        .map_err(|_| anyhow!("Invalid PostgreSQL connection options."))?;
    Ok(resolved.to_owned())
}

/// Connect and initialize the selected backend before committing bootstrap.
/// Errors are safe to show in a terminal: driver errors can contain credentials
/// and are intentionally not propagated or logged here.
pub async fn configure(home: &Path, choice: StorageChoice) -> anyhow::Result<()> {
    let before = read_toml(home)?;
    ensure!(
        before.get("storage_url").is_none(),
        "Storage configuration changed. Restart ChaOS to use it."
    );
    let (url, reference) = match choice {
        StorageChoice::Postgres(connection) => {
            let connection = connection.trim();
            let url = postgres_url(connection)?;
            (url, Some(connection.to_owned()))
        }
        StorageChoice::Sqlite => {
            // Escape URL metacharacters in the home path, including '?' and '#'.
            let url = url::Url::from_file_path(home.join("chaos.sqlite"))
                .map_err(|_| anyhow!("ChaOS home must be an absolute path."))?;
            std::fs::create_dir_all(home)
                .map_err(|_| anyhow!("Cannot create ChaOS home. Check directory permissions."))?;
            (url.as_str().replacen("file:", "sqlite:", 1), None)
        }
    };
    tokio::time::timeout(
        Duration::from_secs(30),
        crate::runtime_db::open_or_create_runtime_db_with_config(Some(&url), home, "settings"),
    )
    .await
    .map_err(|_| anyhow!("Database connection timed out. Check the server and network, then retry."))?
    .map_err(|_| anyhow!("Cannot initialize the database. Check the connection, credentials, and database permissions, then retry."))?;

    let _lock = lock_bootstrap(home)?;
    ensure!(
        read_toml(home)? == before,
        "Storage configuration changed during setup. Restart ChaOS to use it."
    );
    let mut new_secret = false;
    let stored = match reference {
        Some(reference)
            if reference.starts_with("env:") || chaos_sysctl::secrets::is_reference(&reference) =>
        {
            reference
        }
        Some(_) => {
            let reference = chaos_sysctl::secrets::externalize(&url).map_err(|_| {
                anyhow!("Cannot save the connection in the secure credential store. Use an env:VARIABLE reference instead.")
            })?;
            new_secret = true;
            reference
        }
        None => url,
    };
    if write_bootstrap(home, "storage_url", &stored).is_err() {
        if new_secret {
            let _ = chaos_sysctl::secrets::remove(&stored);
        }
        return Err(anyhow!(
            "Cannot save storage configuration. Check permissions on ChaOS home and retry."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
