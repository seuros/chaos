use crate::live_wrap::take_prefix_by_width;
use chaos_syslog::RuntimeMetricsSummary;
use ratatui::prelude::*;
use ratatui::style::Stylize;

use super::super::render;
use super::cells_basic::PlainHistoryCell;
use super::cells_basic::PrefixedWrappedHistoryCell;
use super::cells_composite::ApprovalDecisionActor;
use super::trait_def::HistoryCell;

// ---------------------------------------------------------------------------
// FinalMessageSeparator
// ---------------------------------------------------------------------------

/// A visual divider between turns, optionally showing how long the assistant worked.
#[derive(Debug)]
pub struct FinalMessageSeparator {
    elapsed_seconds: Option<u64>,
    runtime_metrics: Option<RuntimeMetricsSummary>,
}

impl FinalMessageSeparator {
    pub fn new(
        elapsed_seconds: Option<u64>,
        runtime_metrics: Option<RuntimeMetricsSummary>,
    ) -> Self {
        Self {
            elapsed_seconds,
            runtime_metrics,
        }
    }
}

impl HistoryCell for FinalMessageSeparator {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut label_parts = Vec::new();
        if let Some(elapsed_seconds) = self
            .elapsed_seconds
            .filter(|seconds| *seconds > 60)
            .map(crate::status_indicator_widget::fmt_elapsed_compact)
        {
            label_parts.push(format!("Worked for {elapsed_seconds}"));
        }
        if let Some(metrics_label) = self.runtime_metrics.and_then(render::runtime_metrics_label) {
            label_parts.push(metrics_label);
        }

        if label_parts.is_empty() {
            return vec![Line::from_iter(["─".repeat(width as usize).dim()])];
        }

        let label = format!("─ {} ─", label_parts.join(" • "));
        let (label, _suffix, label_width) = take_prefix_by_width(&label, width as usize);
        vec![
            Line::from_iter([
                label,
                "─".repeat((width as usize).saturating_sub(label_width)),
            ])
            .dim(),
        ]
    }
}

// ---------------------------------------------------------------------------
// new_warning_event
// ---------------------------------------------------------------------------

#[allow(clippy::disallowed_methods)]
pub fn new_warning_event(message: String) -> PrefixedWrappedHistoryCell {
    PrefixedWrappedHistoryCell::new(message.yellow(), "⚠ ".yellow(), "  ")
}

// ---------------------------------------------------------------------------
// new_image_generation_call
// ---------------------------------------------------------------------------

pub fn new_image_generation_call(
    call_id: String,
    revised_prompt: Option<String>,
    saved_to: Option<String>,
) -> PlainHistoryCell {
    let detail = revised_prompt.unwrap_or_else(|| call_id.clone());

    let mut lines: Vec<Line<'static>> = vec![
        vec!["• ".dim(), "Generated Image:".bold()].into(),
        vec!["  └ ".dim(), detail.dim()].into(),
    ];
    if let Some(saved_to) = saved_to {
        lines.push(vec!["  └ ".dim(), format!("Saved to: {saved_to}").dim()].into());
    }

    PlainHistoryCell::new(lines)
}

// ---------------------------------------------------------------------------
// new_info_event / new_error_event
// ---------------------------------------------------------------------------

pub fn new_info_event(message: String, hint: Option<String>) -> PlainHistoryCell {
    let mut line = vec!["• ".dim(), message.into()];
    if let Some(hint) = hint {
        line.push(" ".into());
        line.push(hint.dark_gray());
    }
    let lines: Vec<Line<'static>> = vec![line.into()];
    PlainHistoryCell::new(lines)
}

pub fn new_error_event(message: String) -> PlainHistoryCell {
    let lines: Vec<Line<'static>> = vec![vec![format!("■ {message}").red()].into()];
    PlainHistoryCell::new(lines)
}

// ---------------------------------------------------------------------------
// new_review_status_line
// ---------------------------------------------------------------------------

/// Cyan history cell line showing the current review status.
pub fn new_review_status_line(message: String) -> PlainHistoryCell {
    PlainHistoryCell::new(vec![Line::from(message.cyan())])
}

// ---------------------------------------------------------------------------
// new_approval_decision_cell
// ---------------------------------------------------------------------------

pub fn new_approval_decision_cell(
    command: Vec<String>,
    decision: chaos_ipc::protocol::ReviewDecision,
    actor: ApprovalDecisionActor,
) -> Box<dyn HistoryCell> {
    use chaos_ipc::protocol::NetworkPolicyRuleAction;
    use chaos_ipc::protocol::ReviewDecision::*;

    let (symbol, summary): (Span<'static>, Vec<Span<'static>>) = match decision {
        Approved => {
            let snippet = Span::from(render::exec_snippet(&command)).dim();
            (
                "✔ ".green(),
                vec![
                    actor.subject().into(),
                    "approved".bold(),
                    " chaos to run ".into(),
                    snippet,
                    " this time".bold(),
                ],
            )
        }
        ApprovedExecpolicyAmendment {
            proposed_execpolicy_amendment,
        } => {
            let snippet =
                Span::from(render::exec_snippet(&proposed_execpolicy_amendment.command)).dim();
            (
                "✔ ".green(),
                vec![
                    actor.subject().into(),
                    "approved".bold(),
                    " chaos to always run commands that start with ".into(),
                    snippet,
                ],
            )
        }
        ApprovedForSession => {
            let snippet = Span::from(render::exec_snippet(&command)).dim();
            (
                "✔ ".green(),
                vec![
                    actor.subject().into(),
                    "approved".bold(),
                    " chaos to run ".into(),
                    snippet,
                    " every time this session".bold(),
                ],
            )
        }
        NetworkPolicyAmendment {
            network_policy_amendment,
        } => match network_policy_amendment.action {
            NetworkPolicyRuleAction::Allow => (
                "✔ ".green(),
                vec![
                    actor.subject().into(),
                    "persisted".bold(),
                    " Chaos network access to ".into(),
                    Span::from(network_policy_amendment.host).dim(),
                ],
            ),
            NetworkPolicyRuleAction::Deny => (
                "✗ ".red(),
                vec![
                    actor.subject().into(),
                    "denied".bold(),
                    " chaos network access to ".into(),
                    Span::from(network_policy_amendment.host).dim(),
                    " and saved that rule".into(),
                ],
            ),
        },
        Denied => {
            let snippet = Span::from(render::exec_snippet(&command)).dim();
            let summary = match actor {
                ApprovalDecisionActor::User => vec![
                    actor.subject().into(),
                    "did not approve".bold(),
                    " chaos to run ".into(),
                    snippet,
                ],
            };
            ("✗ ".red(), summary)
        }
        Abort => {
            let snippet = Span::from(render::exec_snippet(&command)).dim();
            (
                "✗ ".red(),
                vec![
                    actor.subject().into(),
                    "canceled".bold(),
                    " the request to run ".into(),
                    snippet,
                ],
            )
        }
    };

    Box::new(PrefixedWrappedHistoryCell::new(
        Line::from(summary),
        symbol,
        "  ",
    ))
}
