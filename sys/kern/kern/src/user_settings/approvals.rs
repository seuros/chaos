//! Installation identity and remembered approvals.
use anyhow::Context;
use chaos_proc::{ApprovalState, RememberedApproval};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
use uuid::Uuid;

use super::open;

/// Installation IDs are local application state, not database content or export.
pub fn installation_id(home: &Path) -> anyhow::Result<String> {
    std::fs::create_dir_all(home)?;
    let path = home.join("installation-id");
    if let Ok(id) = std::fs::read_to_string(&path) {
        Uuid::parse_str(id.trim())
            .context("invalid installation identity; do not adopt database grants")?;
        return Ok(id.trim().to_owned());
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            let id = Uuid::new_v4().to_string();
            writeln!(file, "{id}")?;
            file.sync_all()?;
            Ok(id)
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let id = std::fs::read_to_string(path)?;
            Uuid::parse_str(id.trim()).context("installation identity is not ready; retry")?;
            Ok(id.trim().into())
        }
        Err(err) => Err(err.into()),
    }
}

pub fn fingerprint(value: &serde_json::Value) -> anyhow::Result<String> {
    // Canonicalize maps regardless of serde_json's preserve_order feature.
    fn canonical(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let sorted: std::collections::BTreeMap<_, _> =
                    map.iter().map(|(k, v)| (k.clone(), canonical(v))).collect();
                serde_json::Value::Object(sorted.into_iter().collect())
            }
            serde_json::Value::Array(values) => values.iter().map(canonical).collect(),
            _ => value.clone(),
        }
    }
    Ok(format!(
        "v1:{}",
        digest_hex(&serde_json::to_vec(&canonical(value))?)
    ))
}

fn digest_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn workspace_scope(cwd: &Path) -> anyhow::Result<String> {
    let cwd = std::fs::canonicalize(cwd)?;
    let root = cwd
        .ancestors()
        .find(|path| path.join(".git").exists())
        .unwrap_or(&cwd);
    Ok(root.to_string_lossy().into_owned())
}

pub(crate) async fn put_scoped_approval(
    home: &Path,
    cwd: &Path,
    kind: &str,
    payload: serde_json::Value,
) -> anyhow::Result<()> {
    let subject = fingerprint(&payload)?;
    open(home)
        .await?
        .put_approval(&RememberedApproval {
            id: Uuid::new_v4().to_string(),
            installation_id: installation_id(home)?,
            scope: workspace_scope(cwd)?,
            kind: kind.into(),
            subject,
            identity: "v1".into(),
            state: ApprovalState::Active,
            payload,
        })
        .await
}

pub(crate) async fn shell_policy(home: &Path, cwd: &Path) -> anyhow::Result<chaos_selinux::Policy> {
    let scope = workspace_scope(cwd)?;
    let mut policy = chaos_selinux::Policy::empty();
    for grant in open(home)
        .await?
        .list_approvals(&installation_id(home)?)
        .await?
    {
        if grant.kind == "shell" && grant.scope == scope && grant.state == ApprovalState::Active {
            let prefix = Vec::<String>::deserialize(&grant.payload["prefix"])?;
            policy.add_prefix_rule(&prefix, chaos_selinux::Decision::Allow)?;
        }
    }
    Ok(policy)
}

/// User restrictions stored in the database. They can only narrow authority.
pub(crate) async fn database_user_restrictions(
    home: &Path,
) -> anyhow::Result<chaos_selinux::Policy> {
    let runtime = open(home).await?;
    let sources: Vec<String> = serde_json::from_value(runtime.user_policy_sources().await?)?;
    let mut policy = chaos_selinux::Policy::empty();
    for source in sources {
        let mut parser = chaos_selinux::PolicyParser::new();
        parser.parse("database user restrictions", &source)?;
        let parsed = parser.build();
        anyhow::ensure!(
            parsed.host_executables().is_empty(),
            "user policy cannot redefine executable identities"
        );
        policy = policy.merge_overlay(&parsed.restrictions_only());
    }
    Ok(policy)
}
