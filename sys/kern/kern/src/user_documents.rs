//! Named user documents in the runtime database: custom modes, prompts,
//! syntax themes, and halluacinate scripts. Each kind validates its own
//! document shape at import; reads skip disabled rows.
use anyhow::{Context, bail, ensure};
pub use chaos_proc::{DocumentKind, StoredDocument};
use std::path::Path;

pub fn validate_name(name: &str) -> anyhow::Result<()> {
    ensure!(
        (1..=64).contains(&name.len())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "document names use 1-64 characters from [A-Za-z0-9-_.]"
    );
    Ok(())
}

/// Reject documents that would not load, before they are stored.
pub fn validate(kind: DocumentKind, name: &str, source: &str) -> anyhow::Result<()> {
    validate_name(name)?;
    match kind {
        DocumentKind::Mode => {
            let id = crate::modes::custom_mode_id(source)?;
            ensure!(
                id == name,
                "mode document declares id `{id}`; store it under that name"
            );
        }
        DocumentKind::Prompt => {
            ensure!(
                !crate::custom_prompts::parse_frontmatter(source)
                    .2
                    .trim()
                    .is_empty(),
                "prompt body must not be empty"
            );
        }
        DocumentKind::Theme => {
            ensure!(
                source.contains("<plist"),
                "theme documents are tmTheme property lists"
            );
        }
        DocumentKind::Script => {
            ensure!(!source.trim().is_empty(), "script must not be empty");
        }
    }
    Ok(())
}

pub async fn list(home: &Path, kind: DocumentKind) -> anyhow::Result<Vec<StoredDocument>> {
    super::user_settings::open(home)
        .await?
        .list_documents(kind)
        .await
}

/// Enabled documents only, in name order.
pub async fn enabled(home: &Path, kind: DocumentKind) -> anyhow::Result<Vec<StoredDocument>> {
    Ok(list(home, kind)
        .await?
        .into_iter()
        .filter(|document| document.enabled)
        .collect())
}

pub async fn get(
    home: &Path,
    kind: DocumentKind,
    name: &str,
) -> anyhow::Result<Option<StoredDocument>> {
    super::user_settings::open(home)
        .await?
        .get_document(kind, name)
        .await
}

/// Store or replace a document after validating it. Returns the new revision.
pub async fn import(
    home: &Path,
    kind: DocumentKind,
    name: &str,
    source: &str,
) -> anyhow::Result<i64> {
    validate(kind, name, source)?;
    let runtime = super::user_settings::open(home).await?;
    let revision = runtime
        .get_document(kind, name)
        .await?
        .map(|document| document.revision)
        .unwrap_or(0);
    runtime.put_document(kind, name, source, revision).await
}

pub async fn remove(home: &Path, kind: DocumentKind, name: &str) -> anyhow::Result<()> {
    let runtime = super::user_settings::open(home).await?;
    let document = runtime
        .get_document(kind, name)
        .await?
        .with_context(|| format!("no stored {} named `{name}`", kind.as_str()))?;
    runtime.delete_document(kind, name, document.revision).await
}

pub async fn set_enabled(
    home: &Path,
    kind: DocumentKind,
    name: &str,
    enabled: bool,
) -> anyhow::Result<i64> {
    let runtime = super::user_settings::open(home).await?;
    let document = runtime
        .get_document(kind, name)
        .await?
        .with_context(|| format!("no stored {} named `{name}`", kind.as_str()))?;
    if document.enabled == enabled {
        bail!(
            "{} `{name}` is already {}",
            kind.as_str(),
            if enabled { "enabled" } else { "disabled" }
        );
    }
    runtime
        .set_document_enabled(kind, name, document.revision, enabled)
        .await
}
