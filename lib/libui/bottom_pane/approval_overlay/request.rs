use std::collections::HashMap;
use std::path::PathBuf;

use chaos_ipc::ProcessId;
use chaos_ipc::mcp::RequestId;
use chaos_ipc::protocol::ElicitationAction;
use chaos_ipc::protocol::FileChange;
use chaos_ipc::protocol::NetworkApprovalContext;
use chaos_ipc::protocol::ReviewDecision;
use chaos_ipc::request_permissions::RequestPermissionProfile;
use crossterm::event::KeyCode;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use url::Url;

use crate::diff_render::DiffSummary;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::render::highlight::highlight_bash_to_lines;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;

use super::format::format_additional_permissions_rule;

/// Request coming from the agent that needs user approval.
#[derive(Clone, Debug)]
pub enum ApprovalRequest {
    Exec {
        process_id: ProcessId,
        process_label: Option<String>,
        id: String,
        command: Vec<String>,
        reason: Option<String>,
        available_decisions: Vec<ReviewDecision>,
        network_approval_context: Option<NetworkApprovalContext>,
        additional_permissions: Option<chaos_ipc::models::PermissionProfile>,
    },
    Permissions {
        process_id: ProcessId,
        process_label: Option<String>,
        call_id: String,
        reason: Option<String>,
        permissions: RequestPermissionProfile,
    },
    ApplyPatch {
        process_id: ProcessId,
        process_label: Option<String>,
        id: String,
        reason: Option<String>,
        cwd: PathBuf,
        changes: HashMap<PathBuf, FileChange>,
    },
    McpElicitation {
        process_id: ProcessId,
        process_label: Option<String>,
        server_name: String,
        request_id: RequestId,
        message: String,
        url: Option<String>,
    },
}

impl ApprovalRequest {
    pub(super) fn process_id(&self) -> ProcessId {
        match self {
            ApprovalRequest::Exec { process_id, .. }
            | ApprovalRequest::Permissions { process_id, .. }
            | ApprovalRequest::ApplyPatch { process_id, .. }
            | ApprovalRequest::McpElicitation { process_id, .. } => *process_id,
        }
    }

    pub(super) fn process_label(&self) -> Option<&str> {
        match self {
            ApprovalRequest::Exec { process_label, .. }
            | ApprovalRequest::Permissions { process_label, .. }
            | ApprovalRequest::ApplyPatch { process_label, .. }
            | ApprovalRequest::McpElicitation { process_label, .. } => process_label.as_deref(),
        }
    }
}

#[derive(Clone)]
pub(super) enum ApprovalDecision {
    Review(ReviewDecision),
    McpElicitation(ElicitationAction),
}

#[derive(Clone)]
pub(super) struct ApprovalOption {
    pub label: String,
    pub decision: ApprovalDecision,
    pub display_shortcut: Option<KeyBinding>,
    pub additional_shortcuts: Vec<KeyBinding>,
}

impl ApprovalOption {
    pub(super) fn shortcuts(&self) -> impl Iterator<Item = KeyBinding> + '_ {
        self.display_shortcut
            .into_iter()
            .chain(self.additional_shortcuts.iter().copied())
    }
}

pub(super) fn approval_footer_hint(request: &ApprovalRequest) -> Line<'static> {
    let mut spans = vec![
        "Press ".into(),
        key_hint::plain(KeyCode::Enter).into(),
        " to confirm or ".into(),
        key_hint::plain(KeyCode::Esc).into(),
        " to cancel".into(),
    ];
    if request.process_label().is_some() {
        spans.extend([
            " or ".into(),
            key_hint::plain(KeyCode::Char('o')).into(),
            " to open process".into(),
        ]);
    }
    Line::from(spans)
}

pub(super) fn build_header(request: &ApprovalRequest) -> Box<dyn Renderable> {
    match request {
        ApprovalRequest::Exec {
            process_label,
            reason,
            command,
            network_approval_context,
            additional_permissions,
            ..
        } => {
            let mut header: Vec<Line<'static>> = Vec::new();
            if let Some(process_label) = process_label {
                header.push(Line::from(vec![
                    "Process: ".into(),
                    process_label.clone().bold(),
                ]));
                header.push(Line::from(""));
            }
            if let Some(reason) = reason {
                header.push(Line::from(vec!["Reason: ".into(), reason.clone().italic()]));
                header.push(Line::from(""));
            }
            if let Some(additional_permissions) = additional_permissions
                && let Some(rule_line) = format_additional_permissions_rule(additional_permissions)
            {
                header.push(Line::from(vec![
                    "Permission rule: ".into(),
                    rule_line.cyan(),
                ]));
                header.push(Line::from(""));
            }
            let full_cmd = strip_bash_lc_and_escape(command);
            let mut full_cmd_lines = highlight_bash_to_lines(&full_cmd);
            if let Some(first) = full_cmd_lines.first_mut() {
                first.spans.insert(0, Span::from("$ "));
            }
            if network_approval_context.is_none() {
                header.extend(full_cmd_lines);
            }
            Box::new(Paragraph::new(header).wrap(Wrap { trim: false }))
        }
        ApprovalRequest::Permissions {
            process_label,
            reason,
            permissions,
            ..
        } => {
            let mut header: Vec<Line<'static>> = Vec::new();
            if let Some(process_label) = process_label {
                header.push(Line::from(vec![
                    "Process: ".into(),
                    process_label.clone().bold(),
                ]));
                header.push(Line::from(""));
            }
            if let Some(reason) = reason {
                header.push(Line::from(vec!["Reason: ".into(), reason.clone().italic()]));
                header.push(Line::from(""));
            }
            if let Some(rule_line) = super::format::format_requested_permissions_rule(permissions) {
                header.push(Line::from(vec![
                    "Permission rule: ".into(),
                    rule_line.cyan(),
                ]));
            }
            Box::new(Paragraph::new(header).wrap(Wrap { trim: false }))
        }
        ApprovalRequest::ApplyPatch {
            process_label,
            reason,
            cwd,
            changes,
            ..
        } => {
            let mut header: Vec<Box<dyn Renderable>> = Vec::new();
            if let Some(process_label) = process_label {
                header.push(Box::new(Line::from(vec![
                    "Process: ".into(),
                    process_label.clone().bold(),
                ])));
                header.push(Box::new(Line::from("")));
            }
            if let Some(reason) = reason
                && !reason.is_empty()
            {
                header.push(Box::new(
                    Paragraph::new(Line::from_iter([
                        "Reason: ".into(),
                        reason.clone().italic(),
                    ]))
                    .wrap(Wrap { trim: false }),
                ));
                header.push(Box::new(Line::from("")));
            }
            header.push(DiffSummary::new(changes.clone(), cwd.clone()).into());
            Box::new(ColumnRenderable::with(header))
        }
        ApprovalRequest::McpElicitation {
            process_label,
            server_name,
            message,
            url,
            ..
        } => {
            let mut lines = Vec::new();
            if let Some(process_label) = process_label {
                lines.push(Line::from(vec![
                    "Process: ".into(),
                    process_label.clone().bold(),
                ]));
                lines.push(Line::from(""));
            }
            lines.extend([
                Line::from(vec!["Server: ".into(), server_name.clone().bold()]),
                Line::from(""),
            ]);
            if let Some(url) = url {
                let host = Url::parse(url)
                    .ok()
                    .and_then(|parsed| parsed.host_str().map(str::to_string));
                if let Some(host) = host {
                    lines.push(Line::from(vec!["Site: ".into(), host.bold()]));
                }
                lines.push(Line::from(vec![
                    "URL: ".into(),
                    url.clone().cyan().underlined(),
                ]));
                lines.push(Line::from(""));
            }
            lines.push(Line::from(message.clone()));
            let header = Paragraph::new(lines).wrap(Wrap { trim: false });
            Box::new(header)
        }
    }
}

pub(super) fn exec_options(
    available_decisions: &[ReviewDecision],
    network_approval_context: Option<&NetworkApprovalContext>,
    additional_permissions: Option<&chaos_ipc::models::PermissionProfile>,
) -> Vec<ApprovalOption> {
    use chaos_ipc::protocol::NetworkPolicyRuleAction;
    available_decisions
        .iter()
        .filter_map(|decision| match decision {
            ReviewDecision::Approved => Some(ApprovalOption {
                label: if network_approval_context.is_some() {
                    "Yes, just this once".to_string()
                } else {
                    "Yes, proceed".to_string()
                },
                decision: ApprovalDecision::Review(ReviewDecision::Approved),
                display_shortcut: None,
                additional_shortcuts: vec![key_hint::plain(KeyCode::Char('y'))],
            }),
            ReviewDecision::ApprovedExecpolicyAmendment {
                proposed_execpolicy_amendment,
            } => {
                let rendered_prefix =
                    strip_bash_lc_and_escape(proposed_execpolicy_amendment.command());
                if rendered_prefix.contains('\n') || rendered_prefix.contains('\r') {
                    return None;
                }

                Some(ApprovalOption {
                    label: format!(
                        "Yes, and don't ask again for commands that start with `{rendered_prefix}`"
                    ),
                    decision: ApprovalDecision::Review(
                        ReviewDecision::ApprovedExecpolicyAmendment {
                            proposed_execpolicy_amendment: proposed_execpolicy_amendment.clone(),
                        },
                    ),
                    display_shortcut: None,
                    additional_shortcuts: vec![key_hint::plain(KeyCode::Char('p'))],
                })
            }
            ReviewDecision::ApprovedForSession => Some(ApprovalOption {
                label: if network_approval_context.is_some() {
                    "Yes, and allow this host for this conversation".to_string()
                } else if additional_permissions.is_some() {
                    "Yes, and allow these permissions for this session".to_string()
                } else {
                    "Yes, and don't ask again for this command in this session".to_string()
                },
                decision: ApprovalDecision::Review(ReviewDecision::ApprovedForSession),
                display_shortcut: None,
                additional_shortcuts: vec![key_hint::plain(KeyCode::Char('a'))],
            }),
            ReviewDecision::NetworkPolicyAmendment {
                network_policy_amendment,
            } => {
                let (label, shortcut) = match network_policy_amendment.action {
                    NetworkPolicyRuleAction::Allow => (
                        "Yes, and allow this host in the future".to_string(),
                        KeyCode::Char('p'),
                    ),
                    NetworkPolicyRuleAction::Deny => (
                        "No, and block this host in the future".to_string(),
                        KeyCode::Char('d'),
                    ),
                };
                Some(ApprovalOption {
                    label,
                    decision: ApprovalDecision::Review(ReviewDecision::NetworkPolicyAmendment {
                        network_policy_amendment: network_policy_amendment.clone(),
                    }),
                    display_shortcut: None,
                    additional_shortcuts: vec![key_hint::plain(shortcut)],
                })
            }
            ReviewDecision::Denied => Some(ApprovalOption {
                label: "No, continue without running it".to_string(),
                decision: ApprovalDecision::Review(ReviewDecision::Denied),
                display_shortcut: None,
                additional_shortcuts: vec![key_hint::plain(KeyCode::Char('d'))],
            }),
            ReviewDecision::Abort => Some(ApprovalOption {
                label: "No, and tell Chaos what to do differently".to_string(),
                decision: ApprovalDecision::Review(ReviewDecision::Abort),
                display_shortcut: Some(key_hint::plain(KeyCode::Esc)),
                additional_shortcuts: vec![key_hint::plain(KeyCode::Char('n'))],
            }),
        })
        .collect()
}

pub(super) fn patch_options() -> Vec<ApprovalOption> {
    vec![
        ApprovalOption {
            label: "Yes, proceed".to_string(),
            decision: ApprovalDecision::Review(ReviewDecision::Approved),
            display_shortcut: None,
            additional_shortcuts: vec![key_hint::plain(KeyCode::Char('y'))],
        },
        ApprovalOption {
            label: "Yes, and don't ask again for these files".to_string(),
            decision: ApprovalDecision::Review(ReviewDecision::ApprovedForSession),
            display_shortcut: None,
            additional_shortcuts: vec![key_hint::plain(KeyCode::Char('a'))],
        },
        ApprovalOption {
            label: "No, and tell Chaos what to do differently".to_string(),
            decision: ApprovalDecision::Review(ReviewDecision::Abort),
            display_shortcut: Some(key_hint::plain(KeyCode::Esc)),
            additional_shortcuts: vec![key_hint::plain(KeyCode::Char('n'))],
        },
    ]
}

pub(super) fn permissions_options() -> Vec<ApprovalOption> {
    vec![
        ApprovalOption {
            label: "Yes, grant these permissions".to_string(),
            decision: ApprovalDecision::Review(ReviewDecision::Approved),
            display_shortcut: None,
            additional_shortcuts: vec![key_hint::plain(KeyCode::Char('y'))],
        },
        ApprovalOption {
            label: "Yes, grant these permissions for this session".to_string(),
            decision: ApprovalDecision::Review(ReviewDecision::ApprovedForSession),
            display_shortcut: None,
            additional_shortcuts: vec![key_hint::plain(KeyCode::Char('a'))],
        },
        ApprovalOption {
            label: "No, continue without permissions".to_string(),
            decision: ApprovalDecision::Review(ReviewDecision::Denied),
            display_shortcut: None,
            additional_shortcuts: vec![key_hint::plain(KeyCode::Char('n'))],
        },
    ]
}

pub(super) fn elicitation_options(is_url_mode: bool) -> Vec<ApprovalOption> {
    if is_url_mode {
        vec![
            ApprovalOption {
                label: "Yes, open the link in my browser".to_string(),
                decision: ApprovalDecision::McpElicitation(ElicitationAction::Accept),
                display_shortcut: None,
                additional_shortcuts: vec![key_hint::plain(KeyCode::Char('y'))],
            },
            ApprovalOption {
                label: "No, don't open it".to_string(),
                decision: ApprovalDecision::McpElicitation(ElicitationAction::Decline),
                display_shortcut: None,
                additional_shortcuts: vec![key_hint::plain(KeyCode::Char('n'))],
            },
            ApprovalOption {
                label: "Cancel this request".to_string(),
                decision: ApprovalDecision::McpElicitation(ElicitationAction::Cancel),
                display_shortcut: Some(key_hint::plain(KeyCode::Esc)),
                additional_shortcuts: vec![key_hint::plain(KeyCode::Char('c'))],
            },
        ]
    } else {
        vec![
            ApprovalOption {
                label: "Yes, provide the requested info".to_string(),
                decision: ApprovalDecision::McpElicitation(ElicitationAction::Accept),
                display_shortcut: None,
                additional_shortcuts: vec![key_hint::plain(KeyCode::Char('y'))],
            },
            ApprovalOption {
                label: "No, but continue without it".to_string(),
                decision: ApprovalDecision::McpElicitation(ElicitationAction::Decline),
                display_shortcut: None,
                additional_shortcuts: vec![key_hint::plain(KeyCode::Char('n'))],
            },
            ApprovalOption {
                label: "Cancel this request".to_string(),
                decision: ApprovalDecision::McpElicitation(ElicitationAction::Cancel),
                display_shortcut: Some(key_hint::plain(KeyCode::Esc)),
                additional_shortcuts: vec![key_hint::plain(KeyCode::Char('c'))],
            },
        ]
    }
}
