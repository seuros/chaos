//! Turn template: precomputed defaults for `Op::UserTurn` submissions.

use std::path::PathBuf;

use chaos_chassis::turn::TurnContext;
use chaos_chassis::turn::TurnSubmission;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_kern::config::Config;

/// Precomputed defaults for [`Op::UserTurn`] submissions.
///
/// Snapshotted from [`Config`] at boot so the composer can submit turns
/// without keeping a live `Config` reference around. Fields are cloned into
/// each submission. When the user's config does not pin a model, an empty
/// string is passed and the kernel surfaces an `Error` event — which is
/// exactly what we want the GUI to render.
#[derive(Debug, Clone)]
pub struct TurnTemplate {
    pub(super) cwd: PathBuf,
    pub(super) approval_policy: ApprovalPolicy,
    pub(super) sandbox_policy: SandboxPolicy,
    pub(super) model: String,
}

impl TurnTemplate {
    /// Extract a snapshot of the fields `Op::UserTurn` requires from a
    /// fully-built [`Config`].
    pub(super) fn from_config(config: &Config) -> Self {
        Self {
            cwd: config.cwd.clone(),
            approval_policy: config.permissions.approval_policy.value(),
            sandbox_policy: config.permissions.sandbox_policy.get().clone(),
            model: config.model.clone().unwrap_or_default(),
        }
    }

    /// Dead-window fallback used only when the iced boot closure is called
    /// more than once. Values chosen so a submission would serialize but
    /// never actually reach a kernel (the window is already inert).
    pub(super) fn fallback() -> Self {
        Self {
            cwd: PathBuf::from("/"),
            approval_policy: ApprovalPolicy::default(),
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: String::new(),
        }
    }

    /// Build a fresh [`Op::UserTurn`] from the template and a user-typed
    /// prompt. Clones per call — the template is meant to be reused.
    pub(super) fn build_turn(&self, prompt: String) -> Op {
        TurnSubmission::text(
            prompt,
            Vec::new(),
            TurnContext {
                cwd: self.cwd.clone(),
                approval_policy: self.approval_policy,
                sandbox_policy: self.sandbox_policy.clone(),
                model: self.model.clone(),
                effort: None,
                summary: None,
                service_tier: None,
                final_output_json_schema: None,
                collaboration_mode: None,
                personality: None,
            },
        )
        .into_op()
    }
}
