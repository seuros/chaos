use std::fs::OpenOptions;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use crate::decision::Decision;
use crate::rule::NetworkRuleProtocol;
use crate::rule::normalize_network_rule_host;
use serde_json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AmendError {
    #[error("prefix rule requires at least one token")]
    EmptyPrefix,
    #[error("invalid network rule: {0}")]
    InvalidNetworkRule(String),
    #[error("policy path has no parent: {path}")]
    MissingParent { path: PathBuf },
    #[error("failed to create policy directory {dir}: {source}")]
    CreatePolicyDir {
        dir: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to format prefix tokens: {source}")]
    SerializePrefix { source: serde_json::Error },
    #[error("failed to serialize network rule field: {source}")]
    SerializeNetworkRule { source: serde_json::Error },
    #[error("failed to open policy file {path}: {source}")]
    OpenPolicyFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write to policy file {path}: {source}")]
    WritePolicyFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to lock policy file {path}: {source}")]
    LockPolicyFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to seek policy file {path}: {source}")]
    SeekPolicyFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read policy file {path}: {source}")]
    ReadPolicyFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read metadata for policy file {path}: {source}")]
    PolicyMetadata {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Note this thread uses advisory file locking and performs blocking I/O, so it should be used with
/// [`tokio::task::spawn_blocking`] when called from an async context.
pub fn blocking_append_allow_prefix_rule(
    policy_path: &Path,
    prefix: &[String],
) -> Result<(), AmendError> {
    if prefix.is_empty() {
        return Err(AmendError::EmptyPrefix);
    }

    let tokens = prefix
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| AmendError::SerializePrefix { source })?;
    let pattern = format!("{{{}}}", tokens.join(", "));
    let rule = format!(r#"prefix_rule {{pattern={pattern}, decision="allow"}}"#);
    append_rule_line(policy_path, &rule)
}

/// Note this function uses advisory file locking and performs blocking I/O, so it should be used
/// with [`tokio::task::spawn_blocking`] when called from an async context.
pub fn blocking_append_network_rule(
    policy_path: &Path,
    host: &str,
    protocol: NetworkRuleProtocol,
    decision: Decision,
    justification: Option<&str>,
) -> Result<(), AmendError> {
    let host = normalize_network_rule_host(host)
        .map_err(|err| AmendError::InvalidNetworkRule(err.to_string()))?;
    if let Some(raw) = justification
        && raw.trim().is_empty()
    {
        return Err(AmendError::InvalidNetworkRule(
            "justification cannot be empty".to_string(),
        ));
    }

    let host = serde_json::to_string(&host)
        .map_err(|source| AmendError::SerializeNetworkRule { source })?;
    let protocol = serde_json::to_string(protocol.as_policy_string())
        .map_err(|source| AmendError::SerializeNetworkRule { source })?;
    let decision = serde_json::to_string(match decision {
        Decision::Allow => "allow",
        Decision::Prompt => "prompt",
        Decision::Forbidden => "deny",
    })
    .map_err(|source| AmendError::SerializeNetworkRule { source })?;

    let mut args = vec![
        format!("host={host}"),
        format!("protocol={protocol}"),
        format!("decision={decision}"),
    ];
    if let Some(justification) = justification {
        let justification = serde_json::to_string(justification)
            .map_err(|source| AmendError::SerializeNetworkRule { source })?;
        args.push(format!("justification={justification}"));
    }
    let rule = format!("network_rule {{{}}}", args.join(", "));
    append_rule_line(policy_path, &rule)
}

fn append_rule_line(policy_path: &Path, rule: &str) -> Result<(), AmendError> {
    let dir = policy_path
        .parent()
        .ok_or_else(|| AmendError::MissingParent {
            path: policy_path.to_path_buf(),
        })?;
    match std::fs::create_dir(dir) {
        Ok(()) => {}
        Err(ref source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(AmendError::CreatePolicyDir {
                dir: dir.to_path_buf(),
                source,
            });
        }
    }

    append_locked_line(policy_path, rule)
}

fn append_locked_line(policy_path: &Path, line: &str) -> Result<(), AmendError> {
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(policy_path)
        .map_err(|source| AmendError::OpenPolicyFile {
            path: policy_path.to_path_buf(),
            source,
        })?;
    file.lock().map_err(|source| AmendError::LockPolicyFile {
        path: policy_path.to_path_buf(),
        source,
    })?;

    file.seek(SeekFrom::Start(0))
        .map_err(|source| AmendError::SeekPolicyFile {
            path: policy_path.to_path_buf(),
            source,
        })?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|source| AmendError::ReadPolicyFile {
            path: policy_path.to_path_buf(),
            source,
        })?;

    if contents.lines().any(|existing| existing == line) {
        return Ok(());
    }

    if !contents.is_empty() && !contents.ends_with('\n') {
        file.write_all(b"\n")
            .map_err(|source| AmendError::WritePolicyFile {
                path: policy_path.to_path_buf(),
                source,
            })?;
    }

    file.write_all(format!("{line}\n").as_bytes())
        .map_err(|source| AmendError::WritePolicyFile {
            path: policy_path.to_path_buf(),
            source,
        })?;

    Ok(())
}

#[cfg(test)]
mod tests;
