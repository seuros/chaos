//! Editor transport use remains underneath native tool admission and policy.
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use chaos_ipc::permissions::VfsPolicy;

use crate::EditorFileAccess;
use crate::chaos::{Session, TurnContext};
use crate::exec::{ExecToolCallOutput, StreamOutput};
use crate::sandboxing::effective_vfs_policy;
use crate::tools::sandboxing::ToolError;

pub(crate) const TEXT_BYTES: usize = 256 * 1024;

struct FileStamp {
    requested: PathBuf,
    canonical: PathBuf,
    metadata: std::fs::Metadata,
}

async fn checked_path(
    policy: &VfsPolicy,
    path: &Path,
    cwd: &Path,
    write: bool,
) -> io::Result<FileStamp> {
    let denied =
        || io::Error::other("editor path is unavailable under the native filesystem policy");
    if !path.is_absolute() || !chaos_parole::sandbox::can_read_path(policy, path, cwd) {
        return Err(denied());
    }
    let canonical = tokio::fs::canonicalize(path).await?;
    let parent = canonical.parent().ok_or_else(denied)?;
    if !chaos_parole::sandbox::can_read_path(policy, &canonical, cwd)
        || !chaos_parole::sandbox::can_read_path(policy, parent, cwd)
        || (write
            && (!chaos_parole::sandbox::can_write_path(policy, &canonical, cwd)
                || !chaos_parole::sandbox::can_write_path(policy, parent, cwd)))
    {
        return Err(denied());
    }
    let metadata = tokio::fs::metadata(&canonical).await?;
    if !metadata.is_file() {
        return Err(io::Error::other(
            "editor access requires an existing regular file",
        ));
    }
    Ok(FileStamp {
        requested: path.to_path_buf(),
        canonical,
        metadata,
    })
}

impl FileStamp {
    async fn recheck(&self, policy: &VfsPolicy, cwd: &Path, write: bool) -> io::Result<()> {
        let fresh = checked_path(policy, &self.requested, cwd, write).await?;
        #[cfg(unix)]
        let same = {
            use std::os::unix::fs::MetadataExt;
            self.metadata.dev() == fresh.metadata.dev()
                && self.metadata.ino() == fresh.metadata.ino()
        };
        #[cfg(not(unix))]
        let same = false;
        if !same || fresh.canonical != self.canonical {
            return Err(io::Error::other(
                "editor file identity changed; request was not replayed",
            ));
        }
        Ok(())
    }
}

async fn bounded_read(access: &dyn EditorFileAccess, path: &Path) -> io::Result<String> {
    let text = access.read(path).await?;
    if text.len() > TEXT_BYTES {
        return Err(io::Error::other(
            "editor text exceeds the native byte budget",
        ));
    }
    Ok(text)
}

pub(crate) async fn read(
    session: &Session,
    turn: &TurnContext,
    access: &dyn EditorFileAccess,
    arguments: &serde_json::Value,
) -> Result<String, String> {
    let path = chaos_arsenal::tools::read_file::requested_path(arguments)?;
    let snapshot = session.permission_snapshot(turn).await;
    let policy = effective_vfs_policy(&snapshot.vfs_policy, snapshot.granted_permissions.as_ref());
    let stamp = checked_path(&policy, &path, &turn.cwd, false)
        .await
        .map_err(|error| error.to_string())?;
    let text = bounded_read(access, &stamp.canonical)
        .await
        .map_err(|error| error.to_string())?;
    stamp
        .recheck(&policy, &turn.cwd, false)
        .await
        .map_err(|error| error.to_string())?;
    let output = chaos_arsenal::tools::read_file::execute_editor_structured(arguments, &text)
        .await
        .map(|value| value.to_string())?;
    if output.len() > TEXT_BYTES {
        return Err("formatted editor output exceeds the native byte budget".into());
    }
    Ok(output)
}

pub(crate) struct PreparedPatch {
    access: Arc<dyn EditorFileAccess>,
    stamp: FileStamp,
    original: String,
}

impl std::fmt::Debug for PreparedPatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedEditorPatch")
            .finish_non_exhaustive()
    }
}

pub(crate) async fn prepare_patch(
    session: &Session,
    turn: &TurnContext,
    access: Arc<dyn EditorFileAccess>,
    patch: &str,
) -> Result<(chaos_diff::ApplyPatchAction, PreparedPatch), String> {
    if !access.can_write() {
        return Err("the editor negotiated read-only text access".into());
    }
    let path = chaos_diff::text_update_path(patch, &turn.cwd).map_err(|error| error.to_string())?;
    let snapshot = session.permission_snapshot(turn).await;
    let policy = effective_vfs_policy(&snapshot.vfs_policy, snapshot.granted_permissions.as_ref());
    let stamp = checked_path(&policy, &path, &turn.cwd, false)
        .await
        .map_err(|e| e.to_string())?;
    let original = bounded_read(access.as_ref(), &stamp.canonical)
        .await
        .map_err(|e| e.to_string())?;
    stamp
        .recheck(&policy, &turn.cwd, false)
        .await
        .map_err(|e| e.to_string())?;
    let action = chaos_diff::prepare_text_update(patch, &turn.cwd, original.clone())
        .map_err(|error| error.to_string())?;
    let size = action.changes().values().find_map(|change| match change {
        chaos_diff::ApplyPatchFileChange::Update { new_content, .. } => Some(new_content.len()),
        _ => None,
    });
    if size.is_none_or(|size| size > TEXT_BYTES) {
        return Err("editor patch exceeds the native text budget".into());
    }
    Ok((
        action,
        PreparedPatch {
            access,
            stamp,
            original,
        },
    ))
}

impl PreparedPatch {
    pub(crate) async fn run(
        &self,
        req: &crate::tools::runtimes::apply_patch::ApplyPatchRequest,
        attempt: &crate::tools::sandboxing::SandboxAttempt<'_>,
    ) -> Result<ExecToolCallOutput, ToolError> {
        let rejected = |error: io::Error| ToolError::Rejected(error.to_string());
        let policy = effective_vfs_policy(
            attempt.file_system_policy,
            req.additional_permissions.as_ref(),
        );
        self.stamp
            .recheck(&policy, attempt.sandbox_cwd, true)
            .await
            .map_err(rejected)?;
        let current = bounded_read(self.access.as_ref(), &self.stamp.canonical)
            .await
            .map_err(rejected)?;
        if current != self.original {
            return Err(ToolError::Rejected(
                "editor text changed after patch preparation; no write was sent".into(),
            ));
        }
        self.stamp
            .recheck(&policy, attempt.sandbox_cwd, true)
            .await
            .map_err(rejected)?;
        let Some(chaos_diff::ApplyPatchFileChange::Update {
            new_content,
            move_path: None,
            ..
        }) = req.action.changes().values().next()
        else {
            return Err(ToolError::Rejected(
                "unsupported editor patch action".into(),
            ));
        };
        let start = Instant::now();
        self.access
            .write(&self.stamp.canonical, new_content.clone())
            .await
            .map_err(rejected)?;
        let mut summary = Vec::new();
        chaos_diff::print_summary(
            &chaos_diff::AffectedPaths {
                added: vec![],
                modified: vec![self.stamp.canonical.clone()],
                deleted: vec![],
            },
            &mut summary,
        )
        .map_err(rejected)?;
        let summary = String::from_utf8_lossy(&summary).into_owned();
        Ok(ExecToolCallOutput {
            exit_code: 0,
            stdout: StreamOutput::new(summary.clone()),
            stderr: StreamOutput::new(String::new()),
            aggregated_output: StreamOutput::new(summary),
            duration: start.elapsed(),
            timed_out: false,
        })
    }
}
