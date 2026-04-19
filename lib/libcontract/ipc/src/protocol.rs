//! Defines the protocol for a Chaos session between a client and an agent.
//!
//! Uses a SQ (Submission Queue) / EQ (Event Queue) pattern to asynchronously communicate
//! between user and agent.

// Re-exported alias used by child modules (collab, session) via `super::`.
use crate::openai_models::ReasoningEffort as ReasoningEffortConfig;

mod collab;
mod errors;
mod event_msg;
mod events_agent;
mod events_execution;
mod events_review;
mod events_token;
mod events_tool;
mod hooks;
mod policy;
mod requests;
mod review;
mod session;
pub use crate::approvals::ApplyPatchApprovalRequestEvent;
pub use crate::approvals::ElicitationAction;
pub use crate::approvals::ElicitationCompleteEvent;
pub use crate::approvals::ExecApprovalRequestEvent;
pub use crate::approvals::ExecPolicyAmendment;
pub use crate::approvals::NetworkApprovalContext;
pub use crate::approvals::NetworkApprovalProtocol;
pub use crate::approvals::NetworkPolicyAmendment;
pub use crate::approvals::NetworkPolicyRuleAction;
pub use crate::permissions::SocketPolicy;
pub use crate::permissions::VfsAccessMode;
pub use crate::permissions::VfsEntry;
pub use crate::permissions::VfsPath;
pub use crate::permissions::VfsPolicy;
pub use crate::permissions::VfsPolicyKind;
pub use crate::permissions::VfsSpecialPath;
pub use crate::request_permissions::RequestPermissionsArgs;
pub use crate::request_user_input::RequestUserInputEvent;
// Re-exported for tests and downstream consumers that use `protocol::*`.
pub use crate::ProcessId;
pub use crate::config_types::ApprovalsReviewer;
pub use crate::config_types::ReasoningSummary as ReasoningSummaryConfig;
pub use crate::user_input::UserInput;
pub use collab::*;
pub use errors::*;
pub use event_msg::*;
pub use events_agent::*;
pub use events_execution::*;
pub use events_review::*;
pub use events_token::*;
pub use events_tool::*;
pub use hooks::*;
pub use policy::*;
pub use requests::*;
pub use review::*;
pub use session::*;

/// Open/close tags for special user-input blocks. Used across crates to avoid
/// duplicated hardcoded strings.
pub const USER_INSTRUCTIONS_OPEN_TAG: &str = "<user_instructions>";
pub const USER_INSTRUCTIONS_CLOSE_TAG: &str = "</user_instructions>";
pub const ENVIRONMENT_CONTEXT_OPEN_TAG: &str = "<environment_context>";
pub const ENVIRONMENT_CONTEXT_CLOSE_TAG: &str = "</environment_context>";
pub const APPS_INSTRUCTIONS_OPEN_TAG: &str = "<apps_instructions>";
pub const APPS_INSTRUCTIONS_CLOSE_TAG: &str = "</apps_instructions>";
pub const PLUGINS_INSTRUCTIONS_OPEN_TAG: &str = "<plugins_instructions>";
pub const PLUGINS_INSTRUCTIONS_CLOSE_TAG: &str = "</plugins_instructions>";
pub const COLLABORATION_MODE_OPEN_TAG: &str = "<collaboration_mode>";
pub const COLLABORATION_MODE_CLOSE_TAG: &str = "</collaboration_mode>";
pub const USER_MESSAGE_BEGIN: &str = "## My request for Chaos:";

macro_rules! impl_fromstr_via_serde {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl std::str::FromStr for $ty {
                type Err = serde_json::Error;
                fn from_str(s: &str) -> Result<Self, Self::Err> {
                    serde_json::from_str(s)
                }
            }
        )+
    };
}

impl_fromstr_via_serde!(VfsPolicy, SocketPolicy);

#[cfg(test)]
mod tests;
