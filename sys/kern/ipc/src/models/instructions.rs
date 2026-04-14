use std::path::Path;

use chaos_selinux::Policy;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

use super::response::ContentItem;
use super::response::ResponseItem;
use crate::config_types::CollaborationMode;
use crate::config_types::SandboxMode;
use crate::protocol::ApprovalPolicy;
use crate::protocol::COLLABORATION_MODE_CLOSE_TAG;
use crate::protocol::COLLABORATION_MODE_OPEN_TAG;
use crate::protocol::GranularApprovalConfig;
use crate::protocol::NetworkAccess;
use crate::protocol::SandboxPolicy;
use crate::protocol::WritableRoot;

pub const BASE_INSTRUCTIONS_DEFAULT: &str = include_str!("../prompts/base_instructions/default.md");

/// Base instructions for the model in a thread. Corresponds to the `instructions` field in the ResponsesAPI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename = "base_instructions", rename_all = "snake_case")]
pub struct BaseInstructions {
    pub text: String,
}

impl Default for BaseInstructions {
    fn default() -> Self {
        Self {
            text: BASE_INSTRUCTIONS_DEFAULT.to_string(),
        }
    }
}

/// Developer-provided guidance that is injected into a turn as a developer role
/// message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename = "developer_instructions", rename_all = "snake_case")]
pub struct DeveloperInstructions {
    text: String,
}

const APPROVAL_POLICY_NEVER: &str = include_str!("../prompts/permissions/approval_policy/never.md");
const APPROVAL_POLICY_UNLESS_TRUSTED: &str =
    include_str!("../prompts/permissions/approval_policy/unless_trusted.md");
const APPROVAL_POLICY_ON_REQUEST_RULE: &str =
    include_str!("../prompts/permissions/approval_policy/on_request_rule.md");
pub(super) const APPROVAL_POLICY_ON_REQUEST_RULE_REQUEST_PERMISSION: &str =
    include_str!("../prompts/permissions/approval_policy/on_request_rule_request_permission.md");

const SANDBOX_MODE_ROOT_ACCESS: &str =
    include_str!("../prompts/permissions/sandbox_mode/root_access.md");
const SANDBOX_MODE_WORKSPACE_WRITE: &str =
    include_str!("../prompts/permissions/sandbox_mode/workspace_write.md");
const SANDBOX_MODE_READ_ONLY: &str =
    include_str!("../prompts/permissions/sandbox_mode/read_only.md");

impl DeveloperInstructions {
    pub fn new<T: Into<String>>(text: T) -> Self {
        Self { text: text.into() }
    }

    pub fn from(
        approval_policy: ApprovalPolicy,
        exec_policy: &Policy,
        exec_permission_approvals_enabled: bool,
        request_permissions_tool_enabled: bool,
    ) -> DeveloperInstructions {
        let with_request_permissions_tool = |text: &str| {
            if request_permissions_tool_enabled {
                format!("{text}\n\n{}", request_permissions_tool_prompt_section())
            } else {
                text.to_string()
            }
        };
        let on_request_instructions = || {
            let on_request_rule = if exec_permission_approvals_enabled {
                APPROVAL_POLICY_ON_REQUEST_RULE_REQUEST_PERMISSION.to_string()
            } else {
                APPROVAL_POLICY_ON_REQUEST_RULE.to_string()
            };
            let mut sections = vec![on_request_rule];
            if request_permissions_tool_enabled {
                sections.push(request_permissions_tool_prompt_section().to_string());
            }
            if let Some(prefixes) = approved_command_prefixes_text(exec_policy) {
                sections.push(format!(
                    "## Approved command prefixes\nThe following prefix rules have already been approved: {prefixes}"
                ));
            }
            sections.join("\n\n")
        };
        let text = match approval_policy {
            ApprovalPolicy::Headless => APPROVAL_POLICY_NEVER.to_string(),
            ApprovalPolicy::Supervised => {
                with_request_permissions_tool(APPROVAL_POLICY_UNLESS_TRUSTED)
            }
            ApprovalPolicy::Interactive => on_request_instructions(),
            ApprovalPolicy::Granular(granular_config) => granular_instructions(
                granular_config,
                exec_policy,
                exec_permission_approvals_enabled,
                request_permissions_tool_enabled,
            ),
        };

        DeveloperInstructions::new(text)
    }

    pub fn into_text(self) -> String {
        self.text
    }

    pub fn concat(self, other: impl Into<DeveloperInstructions>) -> Self {
        let mut text = self.text;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&other.into().text);
        Self { text }
    }

    pub fn model_switch_message(model_instructions: String) -> Self {
        DeveloperInstructions::new(format!(
            "<model_switch>\nThe user was previously using a different model. Please continue the conversation according to the following instructions:\n\n{model_instructions}\n</model_switch>"
        ))
    }

    pub fn personality_spec_message(spec: String) -> Self {
        let message = format!(
            "<personality_spec> The user has requested a new communication style. Future messages should adhere to the following personality: \n{spec} </personality_spec>"
        );
        DeveloperInstructions::new(message)
    }

    pub fn from_policy(
        sandbox_policy: &SandboxPolicy,
        approval_policy: ApprovalPolicy,
        exec_policy: &Policy,
        cwd: &Path,
        exec_permission_approvals_enabled: bool,
        request_permissions_tool_enabled: bool,
    ) -> Self {
        let network_access = if sandbox_policy.has_full_network_access() {
            NetworkAccess::Enabled
        } else {
            NetworkAccess::Restricted
        };

        let (sandbox_mode, writable_roots) = match sandbox_policy {
            SandboxPolicy::RootAccess => (SandboxMode::RootAccess, None),
            SandboxPolicy::ReadOnly { .. } => (SandboxMode::ReadOnly, None),
            SandboxPolicy::ExternalSandbox { .. } => (SandboxMode::RootAccess, None),
            SandboxPolicy::WorkspaceWrite { .. } => {
                let roots = sandbox_policy.get_writable_roots_with_cwd(cwd);
                (SandboxMode::WorkspaceWrite, Some(roots))
            }
        };

        DeveloperInstructions::from_permissions_with_network(
            sandbox_mode,
            network_access,
            approval_policy,
            exec_policy,
            writable_roots,
            exec_permission_approvals_enabled,
            request_permissions_tool_enabled,
        )
    }

    /// Returns developer instructions from a collaboration mode if they exist and are non-empty.
    pub fn from_collaboration_mode(collaboration_mode: &CollaborationMode) -> Option<Self> {
        collaboration_mode
            .settings
            .minion_instructions
            .as_ref()
            .filter(|instructions| !instructions.is_empty())
            .map(|instructions| {
                DeveloperInstructions::new(format!(
                    "{COLLABORATION_MODE_OPEN_TAG}{instructions}{COLLABORATION_MODE_CLOSE_TAG}"
                ))
            })
    }

    pub(super) fn from_permissions_with_network(
        sandbox_mode: SandboxMode,
        network_access: NetworkAccess,
        approval_policy: ApprovalPolicy,
        exec_policy: &Policy,
        writable_roots: Option<Vec<WritableRoot>>,
        exec_permission_approvals_enabled: bool,
        request_permissions_tool_enabled: bool,
    ) -> Self {
        let start_tag = DeveloperInstructions::new("<permissions instructions>");
        let end_tag = DeveloperInstructions::new("</permissions instructions>");
        start_tag
            .concat(DeveloperInstructions::sandbox_text(
                sandbox_mode,
                network_access,
            ))
            .concat(DeveloperInstructions::from(
                approval_policy,
                exec_policy,
                exec_permission_approvals_enabled,
                request_permissions_tool_enabled,
            ))
            .concat(DeveloperInstructions::from_writable_roots(writable_roots))
            .concat(end_tag)
    }

    fn from_writable_roots(writable_roots: Option<Vec<WritableRoot>>) -> Self {
        let Some(roots) = writable_roots else {
            return DeveloperInstructions::new("");
        };

        if roots.is_empty() {
            return DeveloperInstructions::new("");
        }

        let roots_list: Vec<String> = roots
            .iter()
            .map(|r| format!("`{}`", r.root.to_string_lossy()))
            .collect();
        let text = if roots_list.len() == 1 {
            format!(" The writable root is {}.", roots_list[0])
        } else {
            format!(" The writable roots are {}.", roots_list.join(", "))
        };
        DeveloperInstructions::new(text)
    }

    fn sandbox_text(mode: SandboxMode, network_access: NetworkAccess) -> DeveloperInstructions {
        let template = match mode {
            SandboxMode::RootAccess => SANDBOX_MODE_ROOT_ACCESS.trim_end(),
            SandboxMode::WorkspaceWrite => SANDBOX_MODE_WORKSPACE_WRITE.trim_end(),
            SandboxMode::ReadOnly => SANDBOX_MODE_READ_ONLY.trim_end(),
        };
        let text = template.replace("{network_access}", &network_access.to_string());

        DeveloperInstructions::new(text)
    }
}

fn approved_command_prefixes_text(exec_policy: &Policy) -> Option<String> {
    format_allow_prefixes(exec_policy.get_allowed_prefixes())
        .filter(|prefixes| !prefixes.is_empty())
}

pub(super) fn granular_prompt_intro_text() -> &'static str {
    "# Approval Requests\n\nApproval policy is `granular`. Categories set to `false` are automatically rejected instead of prompting the user."
}

pub(super) fn request_permissions_tool_prompt_section() -> &'static str {
    "# request_permissions Tool\n\nThe built-in `request_permissions` tool is available in this session. Invoke it when you need to request additional `network`, `file_system`, or `macos` permissions before later shell-like commands need them. Request only the specific permissions required for the task."
}

fn granular_instructions(
    granular_config: GranularApprovalConfig,
    exec_policy: &Policy,
    exec_permission_approvals_enabled: bool,
    request_permissions_tool_enabled: bool,
) -> String {
    let sandbox_approval_prompts_allowed = granular_config.allows_sandbox_approval();
    let shell_permission_requests_available =
        exec_permission_approvals_enabled && sandbox_approval_prompts_allowed;
    let request_permissions_tool_prompts_allowed =
        request_permissions_tool_enabled && granular_config.allows_request_permissions();
    let categories = [
        Some((
            granular_config.allows_sandbox_approval(),
            "`sandbox_approval`",
        )),
        Some((granular_config.allows_rules_approval(), "`rules`")),
        request_permissions_tool_enabled.then_some((
            granular_config.allows_request_permissions(),
            "`request_permissions`",
        )),
        Some((
            granular_config.allows_mcp_elicitations(),
            "`mcp_elicitations`",
        )),
    ];
    let prompted_categories = categories
        .iter()
        .flatten()
        .filter(|&&(is_allowed, _)| is_allowed)
        .map(|&(_, category)| format!("- {category}"))
        .collect::<Vec<_>>();
    let rejected_categories = categories
        .iter()
        .flatten()
        .filter(|&&(is_allowed, _)| !is_allowed)
        .map(|&(_, category)| format!("- {category}"))
        .collect::<Vec<_>>();

    let mut sections = vec![granular_prompt_intro_text().to_string()];

    if !prompted_categories.is_empty() {
        sections.push(format!(
            "These approval categories may still prompt the user when needed:\n{}",
            prompted_categories.join("\n")
        ));
    }
    if !rejected_categories.is_empty() {
        sections.push(format!(
            "These approval categories are automatically rejected instead of prompting the user:\n{}",
            rejected_categories.join("\n")
        ));
    }

    if shell_permission_requests_available {
        sections.push(APPROVAL_POLICY_ON_REQUEST_RULE_REQUEST_PERMISSION.to_string());
    }

    if request_permissions_tool_prompts_allowed {
        sections.push(request_permissions_tool_prompt_section().to_string());
    }

    if let Some(prefixes) = approved_command_prefixes_text(exec_policy) {
        sections.push(format!(
            "## Approved command prefixes\nThe following prefix rules have already been approved: {prefixes}"
        ));
    }

    sections.join("\n\n")
}

pub(super) const MAX_RENDERED_PREFIXES: usize = 100;
pub(super) const MAX_ALLOW_PREFIX_TEXT_BYTES: usize = 5000;
pub(super) const TRUNCATED_MARKER: &str = "...\n[Some commands were truncated]";

pub fn format_allow_prefixes(prefixes: Vec<Vec<String>>) -> Option<String> {
    let mut truncated = false;
    if prefixes.len() > MAX_RENDERED_PREFIXES {
        truncated = true;
    }

    let mut prefixes = prefixes;
    prefixes.sort_by(|a, b| {
        a.len()
            .cmp(&b.len())
            .then_with(|| prefix_combined_str_len(a).cmp(&prefix_combined_str_len(b)))
            .then_with(|| a.cmp(b))
    });

    let full_text = prefixes
        .into_iter()
        .take(MAX_RENDERED_PREFIXES)
        .map(|prefix| format!("- {}", render_command_prefix(&prefix)))
        .collect::<Vec<_>>()
        .join("\n");

    // truncate to last UTF8 char
    let mut output = full_text;
    let byte_idx = output
        .char_indices()
        .nth(MAX_ALLOW_PREFIX_TEXT_BYTES)
        .map(|(i, _)| i);
    if let Some(byte_idx) = byte_idx {
        truncated = true;
        output = output[..byte_idx].to_string();
    }

    if truncated {
        Some(format!("{output}{TRUNCATED_MARKER}"))
    } else {
        Some(output)
    }
}

fn prefix_combined_str_len(prefix: &[String]) -> usize {
    prefix.iter().map(String::len).sum()
}

fn render_command_prefix(prefix: &[String]) -> String {
    let tokens = prefix
        .iter()
        .map(|token| serde_json::to_string(token).unwrap_or_else(|_| format!("{token:?}")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{tokens}]")
}

impl From<DeveloperInstructions> for ResponseItem {
    fn from(di: DeveloperInstructions) -> Self {
        ResponseItem::Message {
            id: None,
            role: "system".to_string(),
            content: vec![ContentItem::InputText {
                text: di.into_text(),
            }],
            end_turn: None,
            phase: None,
        }
    }
}

impl From<SandboxMode> for DeveloperInstructions {
    fn from(mode: SandboxMode) -> Self {
        let network_access = match mode {
            SandboxMode::RootAccess => NetworkAccess::Enabled,
            SandboxMode::WorkspaceWrite | SandboxMode::ReadOnly => NetworkAccess::Restricted,
        };

        DeveloperInstructions::sandbox_text(mode, network_access)
    }
}
