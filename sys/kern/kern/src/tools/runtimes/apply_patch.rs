//! Apply Patch runtime: executes verified patches under the orchestrator.
//!
//! On Linux, patches are applied via fork+landlock+direct-call: the harness
//! forks a child process, applies landlock sandbox restrictions in the child,
//! then calls `apply_action()` directly as a library function. No subprocess
//! CLI, no re-parsing. The child is disposable — landlock restrictions die
//! with it.
//!
//! On non-Linux platforms, `apply_action()` is called directly in-process
//! without sandboxing. Each platform will get its own alcatraz sandbox
//! implementation (alcatraz-macos, alcatraz-freebsd, etc.).
use crate::exec::ExecToolCallOutput;
use crate::exec::StreamOutput;
use crate::guardian::GuardianApprovalRequest;
use crate::guardian::review_approval_request;
use crate::guardian::routes_approval_to_guardian;
use crate::tools::sandboxing::Approvable;
use crate::tools::sandboxing::ApprovalCtx;
use crate::tools::sandboxing::ExecApprovalRequirement;
use crate::tools::sandboxing::SandboxAttempt;
use crate::tools::sandboxing::Sandboxable;
use crate::tools::sandboxing::SandboxablePreference;
use crate::tools::sandboxing::ToolCtx;
use crate::tools::sandboxing::ToolError;
use crate::tools::sandboxing::ToolRuntime;
use crate::tools::sandboxing::with_cached_approval;
use chaos_diff::ApplyPatchAction;
use chaos_ipc::models::PermissionProfile;
use chaos_ipc::protocol::AskForApproval;
use chaos_ipc::protocol::FileChange;
use chaos_ipc::protocol::ReviewDecision;
use chaos_realpath::AbsolutePathBuf;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Debug)]
pub struct ApplyPatchRequest {
    pub action: ApplyPatchAction,
    pub file_paths: Vec<AbsolutePathBuf>,
    pub changes: std::collections::HashMap<PathBuf, FileChange>,
    pub exec_approval_requirement: ExecApprovalRequirement,
    pub additional_permissions: Option<PermissionProfile>,
    pub permissions_preapproved: bool,
    pub timeout_ms: Option<u64>,
}

#[derive(Default)]
pub struct ApplyPatchRuntime;

impl ApplyPatchRuntime {
    pub fn new() -> Self {
        Self
    }

    fn build_guardian_review_request(
        req: &ApplyPatchRequest,
        call_id: &str,
    ) -> GuardianApprovalRequest {
        GuardianApprovalRequest::ApplyPatch {
            id: call_id.to_string(),
            cwd: req.action.cwd.clone(),
            files: req.file_paths.clone(),
            change_count: req.changes.len(),
            patch: req.action.patch.clone(),
        }
    }

    /// Call `apply_action()` directly without sandboxing. Used on non-Linux
    /// platforms and when the orchestrator retries with `SandboxType::None`.
    fn run_unsandboxed(req: &ApplyPatchRequest) -> Result<ExecToolCallOutput, ToolError> {
        let start = Instant::now();
        match chaos_diff::apply_action(&req.action) {
            Ok(summary) => Ok(ExecToolCallOutput {
                exit_code: 0,
                stdout: StreamOutput::new(summary.clone()),
                stderr: StreamOutput::new(String::new()),
                aggregated_output: StreamOutput::new(summary),
                duration: start.elapsed(),
                timed_out: false,
            }),
            Err(e) => {
                let stderr = e.to_string();
                Ok(ExecToolCallOutput {
                    exit_code: 1,
                    stdout: StreamOutput::new(String::new()),
                    stderr: StreamOutput::new(stderr.clone()),
                    aggregated_output: StreamOutput::new(stderr),
                    duration: start.elapsed(),
                    timed_out: false,
                })
            }
        }
    }
}

impl Sandboxable for ApplyPatchRuntime {
    fn sandbox_preference(&self) -> SandboxablePreference {
        SandboxablePreference::Auto
    }
    fn escalate_on_failure(&self) -> bool {
        true
    }
}

impl Approvable<ApplyPatchRequest> for ApplyPatchRuntime {
    type ApprovalKey = AbsolutePathBuf;

    fn approval_keys(&self, req: &ApplyPatchRequest) -> Vec<Self::ApprovalKey> {
        req.file_paths.clone()
    }

    fn start_approval_async<'a>(
        &'a mut self,
        req: &'a ApplyPatchRequest,
        ctx: ApprovalCtx<'a>,
    ) -> BoxFuture<'a, ReviewDecision> {
        let session = ctx.session;
        let turn = ctx.turn;
        let call_id = ctx.call_id.to_string();
        let retry_reason = ctx.retry_reason.clone();
        let approval_keys = self.approval_keys(req);
        let changes = req.changes.clone();
        Box::pin(async move {
            if routes_approval_to_guardian(turn) {
                let action = ApplyPatchRuntime::build_guardian_review_request(req, ctx.call_id);
                return review_approval_request(session, turn, action, retry_reason).await;
            }
            if req.permissions_preapproved && retry_reason.is_none() {
                return ReviewDecision::Approved;
            }
            if let Some(reason) = retry_reason {
                let rx_approve = session
                    .request_patch_approval(
                        turn,
                        call_id,
                        changes.clone(),
                        Some(reason),
                        /*grant_root*/ None,
                    )
                    .await;
                return rx_approve.await.unwrap_or_default();
            }

            with_cached_approval(
                &session.services,
                "apply_patch",
                approval_keys,
                || async move {
                    let rx_approve = session
                        .request_patch_approval(
                            turn, call_id, changes, /*reason*/ None, /*grant_root*/ None,
                        )
                        .await;
                    rx_approve.await.unwrap_or_default()
                },
            )
            .await
        })
    }

    fn wants_no_sandbox_approval(&self, policy: AskForApproval) -> bool {
        match policy {
            AskForApproval::Never => false,
            AskForApproval::Granular(granular_config) => granular_config.allows_sandbox_approval(),
            AskForApproval::OnFailure => true,
            AskForApproval::OnRequest => true,
            AskForApproval::UnlessTrusted => true,
        }
    }

    // apply_patch approvals are decided upstream by assess_patch_safety.
    //
    // This override ensures the orchestrator runs the patch approval flow when required instead
    // of falling back to the global exec approval policy.
    fn exec_approval_requirement(
        &self,
        req: &ApplyPatchRequest,
    ) -> Option<ExecApprovalRequirement> {
        Some(req.exec_approval_requirement.clone())
    }
}

impl ToolRuntime<ApplyPatchRequest, ExecToolCallOutput> for ApplyPatchRuntime {
    async fn run(
        &mut self,
        req: &ApplyPatchRequest,
        attempt: &SandboxAttempt<'_>,
        _ctx: &ToolCtx,
    ) -> Result<ExecToolCallOutput, ToolError> {
        // When the orchestrator retries with SandboxType::None (e.g. after an
        // approved escalation), skip sandboxing entirely.
        if attempt.sandbox == crate::exec::SandboxType::None {
            return Self::run_unsandboxed(req);
        }

        #[cfg(target_os = "linux")]
        {
            return self.run_forked(req, attempt).await;
        }

        // Non-Linux: call apply_action() directly. Each platform will get its
        // own alcatraz sandbox crate (alcatraz-macos, alcatraz-freebsd, etc.).
        #[cfg(not(target_os = "linux"))]
        {
            let _ = attempt;
            Self::run_unsandboxed(req)
        }
    }
}

/// Fork-based apply_patch for Linux: fork a child, apply landlock+seccomp
/// in the child, call `apply_action()` directly, collect result via pipe.
///
/// This avoids subprocess CLI serialization and redundant patch re-parsing.
/// The child process is disposable — landlock restrictions die with it.
#[cfg(target_os = "linux")]
impl ApplyPatchRuntime {
    async fn run_forked(
        &self,
        req: &ApplyPatchRequest,
        attempt: &SandboxAttempt<'_>,
    ) -> Result<ExecToolCallOutput, ToolError> {
        use crate::error::CodexErr;
        use crate::error::SandboxErr;
        use crate::sandboxing::EffectiveSandboxPermissions;
        use std::io::Read as _;
        use std::io::Write as _;
        use std::os::fd::FromRawFd;

        // Merge request-specific additional_permissions into the turn-wide
        // policy so that approved extra write roots are reflected in landlock.
        let effective = EffectiveSandboxPermissions::new(
            attempt.policy,
            None, // no macOS seatbelt on Linux
            req.additional_permissions.as_ref(),
        );
        let sandbox_policy = &effective.sandbox_policy;
        let network_policy = attempt.network_policy;
        // Resolve writable roots against the turn cwd (sandbox_cwd), not
        // req.action.cwd which may be a nested subdirectory.
        let cwd = attempt.sandbox_cwd;
        let sandbox_type = attempt.sandbox;

        // Create a pipe for the child to send results back.
        let (read_fd, write_fd) = nix_pipe()
            .map_err(|e| ToolError::Rejected(format!("failed to create pipe for fork: {e}")))?;

        let start = Instant::now();

        // Serialize the action before fork — after fork we must be minimal.
        let action_bytes = serde_json::to_vec(&PatchActionTransfer::from_action(&req.action))
            .map_err(|e| {
                unsafe {
                    libc::close(read_fd);
                    libc::close(write_fd);
                }
                ToolError::Rejected(format!("failed to serialize patch action: {e}"))
            })?;

        // SAFETY: We fork, then the child immediately does synchronous work
        // (apply landlock, write files, write to pipe, _exit). No tokio, no
        // allocator tricks, no mutexes. All async-signal-safe or simple fs ops.
        let pid = unsafe { libc::fork() };

        if pid < 0 {
            unsafe {
                libc::close(read_fd);
                libc::close(write_fd);
            }
            return Err(ToolError::Rejected(format!(
                "fork() failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        if pid == 0 {
            // === CHILD PROCESS ===
            // Close read end.
            unsafe { libc::close(read_fd) };

            // Apply landlock+seccomp sandbox.
            let sandbox_result = alcatraz_linux::landlock::apply_sandbox_policy_to_current_thread(
                sandbox_policy,
                network_policy,
                cwd,
                true,  // apply_landlock_fs
                false, // allow_network_for_proxy
                false, // proxy_routed_network
            );

            let result = match sandbox_result {
                Ok(()) => {
                    // Deserialize and apply.
                    match serde_json::from_slice::<PatchActionTransfer>(&action_bytes) {
                        Ok(transfer) => {
                            let action = transfer.into_action();
                            match chaos_diff::apply_action(&action) {
                                Ok(summary) => ChildResult {
                                    exit_code: 0,
                                    stdout: summary,
                                    stderr: String::new(),
                                },
                                Err(e) => ChildResult {
                                    exit_code: 1,
                                    stdout: String::new(),
                                    stderr: e.to_string(),
                                },
                            }
                        }
                        Err(e) => ChildResult {
                            exit_code: 1,
                            stdout: String::new(),
                            stderr: format!("deserialize error: {e}"),
                        },
                    }
                }
                Err(e) => ChildResult {
                    exit_code: 1,
                    stdout: String::new(),
                    stderr: format!("sandbox setup failed: {e}"),
                },
            };

            // Write result to pipe.
            let mut pipe = unsafe { std::fs::File::from_raw_fd(write_fd) };
            let _ = serde_json::to_writer(&mut pipe, &result);
            let _ = pipe.flush();
            drop(pipe);

            // _exit to avoid running destructors in the forked child.
            unsafe { libc::_exit(result.exit_code) };
        }

        // === PARENT PROCESS ===
        unsafe { libc::close(write_fd) };

        // Read result from pipe.
        let mut pipe = unsafe { std::fs::File::from_raw_fd(read_fd) };
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        drop(pipe);

        // Wait for child, honoring timeout_ms if set.
        let deadline = req
            .timeout_ms
            .map(|ms| start + std::time::Duration::from_millis(ms));
        let (exit_code, timed_out) = wait_for_child(pid, deadline);

        let duration = start.elapsed();

        let result: ChildResult = serde_json::from_slice(&buf).unwrap_or(ChildResult {
            exit_code,
            stdout: String::new(),
            stderr: String::from_utf8_lossy(&buf).into_owned(),
        });

        let stdout_text = result.stdout.clone();
        let stderr_text = result.stderr.clone();
        let aggregated = if stderr_text.is_empty() {
            stdout_text.clone()
        } else {
            format!("{stdout_text}{stderr_text}")
        };

        let output = ExecToolCallOutput {
            exit_code: result.exit_code,
            stdout: StreamOutput::new(stdout_text),
            stderr: StreamOutput::new(stderr_text),
            aggregated_output: StreamOutput::new(aggregated),
            duration,
            timed_out,
        };

        // If the child failed and the output looks like a sandbox denial,
        // propagate as SandboxErr::Denied so the orchestrator can trigger
        // the approval/retry flow.
        if crate::exec::is_likely_sandbox_denied(sandbox_type, &output) {
            return Err(ToolError::Codex(CodexErr::Sandbox(SandboxErr::Denied {
                output: Box::new(output),
                network_policy_decision: None,
            })));
        }

        Ok(output)
    }
}

/// Create a Unix pipe, returning (read_fd, write_fd).
#[cfg(target_os = "linux")]
fn nix_pipe() -> std::io::Result<(i32, i32)> {
    let mut fds = [0i32; 2];
    let ret = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((fds[0], fds[1]))
}

/// Wait for a child process, optionally with a deadline. Returns
/// `(exit_code, timed_out)`. If the deadline expires, the child is
/// killed with SIGKILL.
#[cfg(target_os = "linux")]
fn wait_for_child(pid: i32, deadline: Option<Instant>) -> (i32, bool) {
    let Some(deadline) = deadline else {
        // No timeout — blocking wait.
        let mut status: i32 = 0;
        unsafe { libc::waitpid(pid, &mut status, 0) };
        let code = if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else {
            1
        };
        return (code, false);
    };

    // Poll with WNOHANG until the child exits or the deadline passes.
    loop {
        let mut status: i32 = 0;
        let ret = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if ret > 0 {
            let code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else {
                1
            };
            return (code, false);
        }
        if Instant::now() >= deadline {
            // Timeout — kill the child.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            unsafe { libc::waitpid(pid, &mut status, 0) };
            return (1, true);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Minimal transfer struct for sending patch action across fork boundary.
/// We serialize before fork and deserialize in child to avoid sharing
/// heap pointers across the fork.
#[cfg(target_os = "linux")]
#[derive(serde::Serialize, serde::Deserialize)]
struct PatchActionTransfer {
    changes: HashMap<PathBuf, PatchChangeTransfer>,
    cwd: PathBuf,
    patch: String,
}

#[cfg(target_os = "linux")]
#[derive(serde::Serialize, serde::Deserialize)]
enum PatchChangeTransfer {
    Add {
        content: String,
    },
    Delete {
        content: String,
    },
    Update {
        unified_diff: String,
        move_path: Option<PathBuf>,
        new_content: String,
    },
}

#[cfg(target_os = "linux")]
impl PatchActionTransfer {
    fn from_action(action: &ApplyPatchAction) -> Self {
        let changes = action
            .changes()
            .iter()
            .map(|(path, change)| {
                let transfer = match change {
                    chaos_diff::ApplyPatchFileChange::Add { content } => {
                        PatchChangeTransfer::Add {
                            content: content.clone(),
                        }
                    }
                    chaos_diff::ApplyPatchFileChange::Delete { content } => {
                        PatchChangeTransfer::Delete {
                            content: content.clone(),
                        }
                    }
                    chaos_diff::ApplyPatchFileChange::Update {
                        unified_diff,
                        move_path,
                        new_content,
                    } => PatchChangeTransfer::Update {
                        unified_diff: unified_diff.clone(),
                        move_path: move_path.clone(),
                        new_content: new_content.clone(),
                    },
                };
                (path.clone(), transfer)
            })
            .collect();
        Self {
            changes,
            cwd: action.cwd.clone(),
            patch: action.patch.clone(),
        }
    }

    fn into_action(self) -> ApplyPatchAction {
        use chaos_diff::ApplyPatchFileChange;
        let changes = self
            .changes
            .into_iter()
            .map(|(path, transfer)| {
                let change = match transfer {
                    PatchChangeTransfer::Add { content } => ApplyPatchFileChange::Add { content },
                    PatchChangeTransfer::Delete { content } => {
                        ApplyPatchFileChange::Delete { content }
                    }
                    PatchChangeTransfer::Update {
                        unified_diff,
                        move_path,
                        new_content,
                    } => ApplyPatchFileChange::Update {
                        unified_diff,
                        move_path,
                        new_content,
                    },
                };
                (path, change)
            })
            .collect();
        ApplyPatchAction::from_parts(changes, self.cwd, self.patch)
    }
}

/// Result sent from child to parent via pipe.
#[cfg(target_os = "linux")]
#[derive(serde::Serialize, serde::Deserialize)]
struct ChildResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

#[cfg(test)]
#[path = "apply_patch_tests.rs"]
mod tests;
