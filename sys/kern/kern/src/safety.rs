use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use chaos_diff::ApplyPatchAction;
use chaos_diff::ApplyPatchFileChange;
use chaos_parole::sandbox::can_write_path;

use crate::exec::SandboxType;
use crate::util::resolve_path;

use crate::protocol::ApprovalPolicy;
use crate::protocol::VfsPolicy;

#[derive(Debug, PartialEq)]
pub enum SafetyCheck {
    AutoApprove {
        sandbox_type: SandboxType,
        user_explicitly_approved: bool,
    },
    AskUser,
    Reject {
        reason: String,
    },
}

pub fn assess_patch_safety(
    action: &ApplyPatchAction,
    policy: ApprovalPolicy,
    vfs_policy: &VfsPolicy,
    cwd: &Path,
) -> SafetyCheck {
    if action.is_empty() {
        return SafetyCheck::Reject {
            reason: "empty patch".to_string(),
        };
    }

    match policy {
        ApprovalPolicy::Headless | ApprovalPolicy::Interactive | ApprovalPolicy::Granular(_) => {
            // Continue to see if this can be auto-approved.
        }
        ApprovalPolicy::Supervised => {
            return SafetyCheck::AskUser;
        }
    }

    let rejects_sandbox_approval = matches!(policy, ApprovalPolicy::Headless)
        || matches!(
            policy,
            ApprovalPolicy::Granular(granular_config) if !granular_config.sandbox_approval
        );

    // Even though the patch appears to be constrained to writable paths, it is
    // possible that paths in the patch are hard links to files outside the
    // writable roots, so we should still run `apply_patch` in a sandbox in that case.
    if is_write_patch_constrained_to_writable_paths(action, vfs_policy, cwd) {
        if matches!(
            vfs_policy.kind,
            crate::protocol::VfsPolicyKind::Unrestricted
                | crate::protocol::VfsPolicyKind::ExternalSandbox
        ) {
            // RootAccess is intended to bypass sandboxing entirely.
            SafetyCheck::AutoApprove {
                sandbox_type: SandboxType::None,
                user_explicitly_approved: false,
            }
        } else {
            // Only auto‑approve when we can actually enforce a sandbox. Otherwise
            // fall back to asking the user because the patch may touch arbitrary
            // paths outside the project.
            match get_platform_sandbox() {
                Some(sandbox_type) => SafetyCheck::AutoApprove {
                    sandbox_type,
                    user_explicitly_approved: false,
                },
                None => {
                    if rejects_sandbox_approval {
                        SafetyCheck::Reject {
                            reason:
                                "writing outside of the project; rejected by user approval settings"
                                    .to_string(),
                        }
                    } else {
                        SafetyCheck::AskUser
                    }
                }
            }
        }
    } else if rejects_sandbox_approval {
        SafetyCheck::Reject {
            reason: "writing outside of the project; rejected by user approval settings"
                .to_string(),
        }
    } else {
        SafetyCheck::AskUser
    }
}

pub fn get_platform_sandbox() -> Option<SandboxType> {
    if cfg!(target_os = "macos") {
        Some(SandboxType::MacosSeatbelt)
    } else if cfg!(target_os = "linux") {
        Some(SandboxType::LinuxSeccomp)
    } else if cfg!(target_os = "freebsd") {
        Some(SandboxType::FreeBSDCapsicum)
    } else {
        None
    }
}

fn is_write_patch_constrained_to_writable_paths(
    action: &ApplyPatchAction,
    vfs_policy: &VfsPolicy,
    cwd: &Path,
) -> bool {
    // Normalize a path by removing `.` and resolving `..` without touching the
    // filesystem (works even if the file does not exist).
    fn normalize(path: &Path) -> Option<PathBuf> {
        let mut out = PathBuf::new();
        for comp in path.components() {
            match comp {
                Component::ParentDir => {
                    out.pop();
                }
                Component::CurDir => { /* skip */ }
                other => out.push(other.as_os_str()),
            }
        }
        Some(out)
    }

    // Determine whether `path` is inside **any** writable root. Both `path`
    // and roots are converted to absolute, normalized forms before the
    // prefix check.
    let is_path_writable = |p: &PathBuf| {
        let abs = resolve_path(cwd, p);
        let abs = match normalize(&abs) {
            Some(v) => v,
            None => return false,
        };

        can_write_path(vfs_policy, &abs, cwd)
    };

    for (path, change) in action.changes() {
        match change {
            ApplyPatchFileChange::Add { .. } | ApplyPatchFileChange::Delete { .. } => {
                if !is_path_writable(path) {
                    return false;
                }
            }
            ApplyPatchFileChange::Update { move_path, .. } => {
                if !is_path_writable(path) {
                    return false;
                }
                if let Some(dest) = move_path
                    && !is_path_writable(dest)
                {
                    return false;
                }
            }
        }
    }

    true
}

#[cfg(test)]
#[path = "safety_tests.rs"]
mod tests;
