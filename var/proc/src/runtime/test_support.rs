#[cfg(test)]
use chaos_ipc::ProcessId;
#[cfg(test)]
use chaos_ipc::protocol::ApprovalPolicy;
#[cfg(test)]
use chaos_ipc::protocol::SandboxPolicy;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::time::SystemTime;
#[cfg(test)]
use std::time::UNIX_EPOCH;
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
use crate::ProcessMetadata;

#[cfg(test)]
pub(super) fn unique_temp_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "chaos-state-runtime-test-{nanos}-{}",
        Uuid::new_v4()
    ))
}

#[cfg(test)]
pub(super) fn test_process_metadata(
    _codex_home: &Path,
    process_id: ProcessId,
    cwd: PathBuf,
) -> ProcessMetadata {
    let now = jiff::Timestamp::from_second(1_700_000_000).expect("timestamp");
    ProcessMetadata {
        id: process_id,
        created_at: now,
        updated_at: now,
        source: "cli".to_string(),
        agent_nickname: None,
        agent_role: None,
        model_provider: "test-provider".to_string(),
        cwd,
        cli_version: "0.0.0".to_string(),
        title: String::new(),
        sandbox_policy: crate::extract::enum_to_string(&SandboxPolicy::new_read_only_policy()),
        approval_mode: crate::extract::enum_to_string(&ApprovalPolicy::Interactive),
        tokens_used: 0,
        first_user_message: Some("hello".to_string()),
        archived_at: None,
        git_sha: None,
        git_branch: None,
        git_origin_url: None,
    }
}
