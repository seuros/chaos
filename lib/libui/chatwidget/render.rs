//! Ratatui `Renderable` implementation for `ChatWidget`.
//!
//! Rendering is intentionally thin: the widget composes its active cell and the
//! bottom pane into a vertical flex layout and delegates all pixel-level work to
//! those sub-widgets.  This file owns the `impl Renderable` block so the main
//! module stays focused on state and event handling.
//!
//! Status-line, token-usage, and rate-limit display helpers live here because
//! they exist solely to compute and refresh what the user sees in the footer row
//! — they are the visual-state management layer of the widget.

use std::path::Path;
use std::path::PathBuf;

use chaos_halluacinate::StatusLineSpan;
use chaos_ipc::api::ConfigLayerSource;
use chaos_ipc::openai_models::ReasoningEffort as ReasoningEffortConfig;
use chaos_ipc::protocol::CreditsSnapshot;
use chaos_ipc::protocol::RateLimitSnapshot;
use chaos_ipc::protocol::TokenUsage;
use chaos_ipc::protocol::TokenUsageInfo;
use chaos_kern::config_loader::ConfigLayerStackOrdering;
use chaos_kern::git_info::current_branch_name;
use chaos_kern::git_info::get_git_repo_root;
use chaos_pwd::find_chaos_home;
use jiff::Timestamp;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::app_event::AppEvent;
use crate::render::Insets;
use crate::render::renderable::FlexRenderable;
use crate::render::renderable::Renderable;
use crate::render::renderable::RenderableExt;
use crate::render::renderable::RenderableItem;
use crate::status::format_directory_display;
use crate::status::rate_limit_snapshot_display_for_limit;
use crate::status_indicator_widget::STATUS_DETAILS_DEFAULT_MAX_LINES;
use crate::status_indicator_widget::StatusDetailsCapitalization;

use super::ChatWidget;
use super::core::RateLimitSwitchPromptState;
use super::core::StatusIndicatorState;

impl Renderable for ChatWidget {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.as_renderable().render(area, buf);
        self.last_rendered_width.set(Some(area.width as usize));
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.as_renderable().desired_height(width)
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.as_renderable().cursor_pos(area)
    }
}

impl ChatWidget {
    pub(super) fn as_renderable(&self) -> RenderableItem<'_> {
        let active_cell_renderable = match &self.active_cell {
            Some(cell) => RenderableItem::Borrowed(cell).inset(Insets::tlbr(
                /*top*/ 1, /*left*/ 0, /*bottom*/ 0, /*right*/ 0,
            )),
            None => RenderableItem::Owned(Box::new(())),
        };
        let mut flex = FlexRenderable::new();
        flex.push(/*flex*/ 1, active_cell_renderable);
        flex.push(
            /*flex*/ 0,
            RenderableItem::Borrowed(&self.bottom_pane).inset(Insets::tlbr(
                /*top*/ 1, /*left*/ 0, /*bottom*/ 0, /*right*/ 0,
            )),
        );
        RenderableItem::Owned(Box::new(flex))
    }
}

// ── Status indicator ──────────────────────────────────────────────────────────

impl ChatWidget {
    /// Synchronize the bottom-pane "task running" indicator with the current lifecycles.
    ///
    /// The bottom pane only has one running flag, but this module treats it as a derived state of
    /// both the agent turn lifecycle and MCP startup lifecycle.
    pub(super) fn update_task_running_state(&mut self) {
        self.bottom_pane
            .set_task_running(self.agent_turn_running || self.mcp_startup_status.is_some());
    }

    /// Update the status indicator header and details.
    ///
    /// Passing `None` clears any existing details.
    pub(super) fn set_status(
        &mut self,
        header: String,
        details: Option<String>,
        details_capitalization: StatusDetailsCapitalization,
        details_max_lines: usize,
    ) {
        let details = details
            .filter(|details| !details.is_empty())
            .map(|details| {
                let trimmed = details.trim_start();
                match details_capitalization {
                    StatusDetailsCapitalization::CapitalizeFirst => {
                        crate::text_formatting::capitalize_first(trimmed)
                    }
                    StatusDetailsCapitalization::Preserve => trimmed.to_string(),
                }
            });
        self.current_status = StatusIndicatorState {
            header: header.clone(),
            details: details.clone(),
            details_max_lines,
        };
        self.bottom_pane.update_status(
            header,
            details,
            StatusDetailsCapitalization::Preserve,
            details_max_lines,
        );
    }

    /// Convenience wrapper around [`Self::set_status`];
    /// updates the status indicator header and clears any existing details.
    pub(super) fn set_status_header(&mut self, header: String) {
        self.set_status(
            header,
            /*details*/ None,
            StatusDetailsCapitalization::CapitalizeFirst,
            STATUS_DETAILS_DEFAULT_MAX_LINES,
        );
    }

    /// Sets the currently rendered footer status-line value.
    pub fn set_status_line(&mut self, status_line: Option<Line<'static>>) {
        self.bottom_pane.set_status_line(status_line);
    }

    /// Applies a rendered status line from the Lua renderer.
    pub fn set_status_line_script_rendered(
        &mut self,
        process_id: Option<chaos_ipc::ProcessId>,
        generation: u64,
        rendered: bool,
        line: Option<Line<'static>>,
    ) {
        if self.process_id != process_id || self.status_line_script_render_generation != generation
        {
            return;
        }
        let enabled = rendered && line.is_some();
        self.bottom_pane.set_status_line_enabled(enabled);
        self.set_status_line(line);
    }

    pub fn set_halluacinate_handle(
        &mut self,
        halluacinate: Option<chaos_halluacinate::HalluacinateHandle>,
    ) {
        self.status_line_script_render_generation =
            self.status_line_script_render_generation.wrapping_add(1);
        self.halluacinate = halluacinate;
        // Do not render eagerly on attach: callers can attach the handle before
        // the session/state event that should define the statusline context.
        // Normal session/state refresh paths will render once the context is current.
        if self.halluacinate.is_none() {
            self.bottom_pane.set_status_line_enabled(false);
            self.set_status_line(None);
        } else {
            self.refresh_status_line();
        }
    }

    /// Forwards the contextual active-agent label into the bottom-pane footer pipeline.
    ///
    /// `ChatWidget` stays a pass-through here so `App` remains the owner of "which thread is the
    /// user actually looking at?" and the footer stack remains a pure renderer of that decision.
    pub fn set_active_agent_label(&mut self, active_agent_label: Option<String>) {
        self.bottom_pane.set_active_agent_label(active_agent_label);
    }

    /// Recomputes footer status-line content by invoking the Lua statusline renderer.
    ///
    /// The built-in renderer is also Lua-backed, so there is no Rust fallback path here. We only
    /// prepare dynamic context (such as git branch lookup) and then hand the whole render decision
    /// to Halluacinate.
    pub fn refresh_status_line(&mut self) {
        let Some(handle) = self.halluacinate.clone() else {
            self.bottom_pane.set_status_line_enabled(false);
            self.set_status_line(None);
            return;
        };

        self.prepare_status_line_context();
        self.status_line_script_render_generation =
            self.status_line_script_render_generation.wrapping_add(1);
        let generation = self.status_line_script_render_generation;
        let process_id = self.process_id;
        let ctx = self.build_statusline_ctx();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let spans = handle.render_statusline(ctx).await;
            let rendered = spans.is_some();
            let line = spans.and_then(spans_to_line);
            tx.send(AppEvent::StatusLineScriptRendered {
                process_id,
                generation,
                rendered,
                line,
            });
        });
    }

    fn prepare_status_line_context(&mut self) {
        let cwd = self.status_line_cwd().to_path_buf();
        self.sync_status_line_branch_state(&cwd);
        if !self.status_line_branch_lookup_complete {
            self.request_status_line_branch(cwd);
        }
    }

    /// Stores async git-branch lookup results for the current status-line cwd.
    ///
    /// Results are dropped when they target an out-of-date cwd to avoid rendering stale branch
    /// names after directory changes.
    pub fn set_status_line_branch(&mut self, cwd: PathBuf, branch: Option<String>) {
        if self.status_line_branch_cwd.as_ref() != Some(&cwd) {
            self.status_line_branch_pending = false;
            return;
        }
        self.status_line_branch = branch;
        self.status_line_branch_pending = false;
        self.status_line_branch_lookup_complete = true;
    }

    /// Forces a new git-branch lookup for the Lua statusline context.
    pub(super) fn request_status_line_branch_refresh(&mut self) {
        let cwd = self.status_line_cwd().to_path_buf();
        self.sync_status_line_branch_state(&cwd);
        self.request_status_line_branch(cwd);
    }

    pub(super) fn restore_retry_status_header_if_present(&mut self) {
        if let Some(header) = self.retry_status_header.take() {
            self.set_status_header(header);
        }
    }

    pub(super) fn open_theme_picker(&mut self) {
        let chaos_home = find_chaos_home().ok();
        let terminal_width = self
            .last_rendered_width
            .get()
            .and_then(|width| u16::try_from(width).ok());
        let params = crate::theme_picker::build_theme_picker_params(
            self.config.tui_theme.as_deref(),
            chaos_home.as_deref(),
            terminal_width,
        );
        self.bottom_pane.show_selection_view(params);
    }

    fn status_line_cwd(&self) -> &Path {
        self.current_cwd.as_ref().unwrap_or(&self.config.cwd)
    }

    fn status_line_project_root(&self) -> Option<PathBuf> {
        let cwd = self.status_line_cwd();
        if let Some(repo_root) = get_git_repo_root(cwd) {
            return Some(repo_root);
        }

        self.config
            .config_layer_stack
            .get_layers(
                ConfigLayerStackOrdering::LowestPrecedenceFirst,
                /*include_disabled*/ true,
            )
            .iter()
            .find_map(|layer| match &layer.name {
                ConfigLayerSource::Project { dot_codex_folder } => {
                    dot_codex_folder.as_path().parent().map(Path::to_path_buf)
                }
                _ => None,
            })
    }

    fn status_line_project_root_name(&self) -> Option<String> {
        self.status_line_project_root().map(|root| {
            root.file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| format_directory_display(&root, /*max_width*/ None))
        })
    }

    /// Resets git-branch cache state when the status-line cwd changes.
    ///
    /// The branch cache is keyed by cwd because branch lookup is performed relative to that path.
    /// Keeping stale branch values across cwd changes would surface incorrect repository context.
    fn sync_status_line_branch_state(&mut self, cwd: &Path) {
        if self
            .status_line_branch_cwd
            .as_ref()
            .is_some_and(|path| path == cwd)
        {
            return;
        }
        self.status_line_branch_cwd = Some(cwd.to_path_buf());
        self.status_line_branch = None;
        self.status_line_branch_pending = false;
        self.status_line_branch_lookup_complete = false;
    }

    /// Starts an async git-branch lookup unless one is already running.
    ///
    /// The resulting `StatusLineBranchUpdated` event carries the lookup cwd so callers can reject
    /// stale completions after directory changes.
    fn request_status_line_branch(&mut self, cwd: PathBuf) {
        if self.status_line_branch_pending {
            return;
        }
        self.status_line_branch_pending = true;
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let branch = current_branch_name(&cwd).await;
            tx.send(AppEvent::StatusLineBranchUpdated { cwd, branch });
        });
    }

    fn status_line_context_window_size(&self) -> Option<i64> {
        self.token_info
            .as_ref()
            .and_then(|info| info.model_context_window)
            .or(self.config.model_context_window)
    }

    fn status_line_context_remaining_percent(&self) -> Option<i64> {
        let Some(context_window) = self.status_line_context_window_size() else {
            return Some(100);
        };
        let default_usage = TokenUsage::default();
        let usage = self
            .token_info
            .as_ref()
            .map(|info| &info.last_token_usage)
            .unwrap_or(&default_usage);
        Some(
            usage
                .percent_of_context_window_remaining(context_window)
                .clamp(0, 100),
        )
    }

    fn status_line_context_used_percent(&self) -> Option<i64> {
        let remaining = self.status_line_context_remaining_percent().unwrap_or(100);
        Some((100 - remaining).clamp(0, 100))
    }

    fn status_line_total_usage(&self) -> TokenUsage {
        self.token_info
            .as_ref()
            .map(|info| info.total_token_usage.clone())
            .unwrap_or_default()
    }

    fn status_line_last_usage(&self) -> TokenUsage {
        self.token_info
            .as_ref()
            .map(|info| info.last_token_usage.clone())
            .unwrap_or_default()
    }

    fn status_line_reasoning_effort_label(effort: Option<ReasoningEffortConfig>) -> &'static str {
        match effort {
            Some(ReasoningEffortConfig::Minimal) => "minimal",
            Some(ReasoningEffortConfig::Low) => "low",
            Some(ReasoningEffortConfig::Medium) => "medium",
            Some(ReasoningEffortConfig::High) => "high",
            Some(ReasoningEffortConfig::XHigh) => "xhigh",
            None | Some(ReasoningEffortConfig::None) => "default",
        }
    }
}

// ── Token-usage and rate-limit display ───────────────────────────────────────

impl ChatWidget {
    pub fn set_token_info(&mut self, info: Option<TokenUsageInfo>) {
        match info {
            Some(info) => self.apply_token_info(info),
            None => {
                self.bottom_pane
                    .set_context_window(/*percent*/ None, /*used_tokens*/ None);
                self.token_info = None;
            }
        }
    }

    pub(super) fn apply_turn_started_context_window(&mut self, model_context_window: Option<i64>) {
        let info = match self.token_info.take() {
            Some(mut info) => {
                info.model_context_window = model_context_window;
                info
            }
            None => {
                let Some(model_context_window) = model_context_window else {
                    return;
                };
                TokenUsageInfo {
                    total_token_usage: TokenUsage::default(),
                    last_token_usage: TokenUsage::default(),
                    model_context_window: Some(model_context_window),
                }
            }
        };

        self.apply_token_info(info);
    }

    fn apply_token_info(&mut self, info: TokenUsageInfo) {
        let percent = self.context_remaining_percent(&info);
        let used_tokens = self.context_used_tokens(&info, percent.is_some());
        self.bottom_pane.set_context_window(percent, used_tokens);
        self.token_info = Some(info);
    }

    fn context_remaining_percent(&self, info: &TokenUsageInfo) -> Option<i64> {
        info.model_context_window.map(|window| {
            info.last_token_usage
                .percent_of_context_window_remaining(window)
        })
    }

    fn context_used_tokens(&self, info: &TokenUsageInfo, percent_known: bool) -> Option<i64> {
        if percent_known {
            return None;
        }

        Some(info.total_token_usage.tokens_in_context_window())
    }

    pub(super) fn restore_pre_review_token_info(&mut self) {
        if let Some(saved) = self.pre_review_token_info.take() {
            match saved {
                Some(info) => self.apply_token_info(info),
                None => {
                    self.bottom_pane
                        .set_context_window(/*percent*/ None, /*used_tokens*/ None);
                    self.token_info = None;
                }
            }
        }
    }

    pub fn on_rate_limit_snapshot(&mut self, snapshot: Option<RateLimitSnapshot>) {
        if let Some(mut snapshot) = snapshot {
            let limit_id = snapshot
                .limit_id
                .clone()
                .unwrap_or_else(|| "chaos".to_string());
            let limit_label = snapshot
                .limit_name
                .clone()
                .unwrap_or_else(|| limit_id.clone());
            if snapshot.credits.is_none() {
                snapshot.credits = self
                    .rate_limit_snapshots_by_limit_id
                    .get(&limit_id)
                    .and_then(|display| display.credits.as_ref())
                    .map(|credits| CreditsSnapshot {
                        has_credits: credits.has_credits,
                        unlimited: credits.unlimited,
                        balance: credits.balance.clone(),
                    });
            }

            self.plan_type = snapshot.plan_type.or(self.plan_type);

            let is_codex_limit = limit_id.eq_ignore_ascii_case("chaos");
            let warnings = if is_codex_limit {
                self.rate_limit_warnings.take_warnings(
                    snapshot
                        .secondary
                        .as_ref()
                        .map(|window| window.used_percent),
                    snapshot
                        .secondary
                        .as_ref()
                        .and_then(|window| window.window_minutes),
                    snapshot.primary.as_ref().map(|window| window.used_percent),
                    snapshot
                        .primary
                        .as_ref()
                        .and_then(|window| window.window_minutes),
                )
            } else {
                vec![]
            };

            let high_usage = is_codex_limit
                && (snapshot
                    .secondary
                    .as_ref()
                    .map(|w| w.used_percent >= super::core::RATE_LIMIT_SWITCH_PROMPT_THRESHOLD)
                    .unwrap_or(false)
                    || snapshot
                        .primary
                        .as_ref()
                        .map(|w| w.used_percent >= super::core::RATE_LIMIT_SWITCH_PROMPT_THRESHOLD)
                        .unwrap_or(false));

            if high_usage
                && !self.rate_limit_switch_prompt_hidden()
                && self.current_model() != super::core::NUDGE_MODEL_SLUG
                && !matches!(
                    self.rate_limit_switch_prompt,
                    RateLimitSwitchPromptState::Shown
                )
            {
                self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Pending;
            }

            let display =
                rate_limit_snapshot_display_for_limit(&snapshot, limit_label, Timestamp::now());
            self.rate_limit_snapshots_by_limit_id
                .insert(limit_id, display);

            if !warnings.is_empty() {
                for warning in warnings {
                    self.add_to_history(crate::history_cell::new_warning_event(warning));
                }
                self.request_redraw();
            }
        } else {
            self.rate_limit_snapshots_by_limit_id.clear();
        }
        self.refresh_status_line();
    }

    /// Builds the Lua statusline context from current runtime state.
    ///
    /// This is public because cross-crate UI tests in `chaos-console` assert on the exact context
    /// values we hand to Halluacinate.
    pub fn build_statusline_ctx(&self) -> serde_json::Value {
        let model = self.model_display_name().to_string();
        let reasoning_effort =
            Self::status_line_reasoning_effort_label(self.effective_reasoning_effort()).to_string();
        let provider = self.config.model_provider_id.clone();
        let branch = self.status_line_branch.clone();
        let cwd = self.status_line_cwd().to_string_lossy().to_string();
        let cwd_display = format_directory_display(self.status_line_cwd(), /*max_width*/ None);
        let project_root = self.status_line_project_root_name();
        let approval = self.config.permissions.approval_policy.value().to_string();
        let sandbox = self.config.permissions.sandbox_policy.get().to_string();
        let version = crate::version::CHAOS_VERSION;
        let session_id = self.process_id.map(|id| id.to_string());

        let remaining_pct = self.status_line_context_remaining_percent().unwrap_or(100);
        let used_pct = self.status_line_context_used_percent().unwrap_or(0);
        let window_size = self.status_line_context_window_size().unwrap_or(0);

        let token_info_available = self.token_info.is_some();
        let total_usage = self.status_line_total_usage();
        let last_usage = self.status_line_last_usage();

        let total_used = total_usage.tokens_in_context_window();
        let total_effective = total_usage.effective_context_tokens_used();
        let total_blended = total_usage.blended_total();
        let input_tokens = total_usage.input_tokens;
        let output_tokens = total_usage.output_tokens;

        let last_used = last_usage.tokens_in_context_window();
        let last_effective = (total_effective
            - TokenUsage::effective_context_tokens_used_from_total(
                (total_used - last_used).max(0),
            ))
        .max(0);
        let previous_effective = (total_effective - last_effective).max(0);
        let last_blended = last_usage.blended_total();
        let last_input_tokens = last_usage.input_tokens;
        let last_output_tokens = last_usage.output_tokens;

        let chaos_rate_limit = self.rate_limit_snapshots_by_limit_id.get("chaos");
        let rate_limit_captured_at_epoch_seconds =
            chaos_rate_limit.map(|s| s.captured_at.as_second());

        let five_hour = chaos_rate_limit.and_then(|s| s.primary.as_ref()).map(|w| {
            serde_json::json!({
                "used_pct": w.used_percent,
                "remaining_pct": (100.0f64 - w.used_percent).clamp(0.0f64, 100.0f64),
                "window_minutes": w.window_minutes,
                "resets_at_epoch_seconds": w.resets_at_epoch_seconds,
            })
        });

        let weekly = chaos_rate_limit
            .and_then(|s| s.secondary.as_ref())
            .map(|w| {
                serde_json::json!({
                    "used_pct": w.used_percent,
                    "remaining_pct": (100.0f64 - w.used_percent).clamp(0.0f64, 100.0f64),
                    "window_minutes": w.window_minutes,
                    "resets_at_epoch_seconds": w.resets_at_epoch_seconds,
                })
            });

        serde_json::json!({
            "model": model,
            "reasoning_effort": reasoning_effort,
            "provider": provider,
            "branch": branch,
            "cwd": cwd,
            "cwd_display": cwd_display,
            "project_root": project_root,
            "approval": approval,
            "sandbox": sandbox,
            "version": version,
            "session_id": session_id,
            "context": {
                "remaining_pct": remaining_pct,
                "used_pct": used_pct,
                "window_size": window_size,
            },
            "tokens": {
                "available": token_info_available,
                "used": total_used,
                "input": input_tokens,
                "output": output_tokens,
                "blended": total_blended,
                "context_raw": total_used,
                "context_effective": total_effective,
                "has_prior_context": previous_effective > 0,
                "last_raw": last_used,
                "last_effective": last_effective,
                "last_input": last_input_tokens,
                "last_output": last_output_tokens,
                "last_blended": last_blended,
            },
            "five_hour": five_hour,
            "weekly": weekly,
            "rate_limit_captured_at_epoch_seconds": rate_limit_captured_at_epoch_seconds,
        })
    }
}

fn color_from_name(name: &str) -> Color {
    match name.to_ascii_lowercase().as_str() {
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "cyan" => Color::Cyan,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "white" => Color::White,
        "gray" | "grey" => Color::Gray,
        _ => Color::Reset,
    }
}

fn spans_to_line(spans: Vec<StatusLineSpan>) -> Option<Line<'static>> {
    let mut rendered: Vec<Span<'static>> = Vec::new();

    for span in spans {
        if span.line_break {
            if !rendered.is_empty() {
                rendered.push(Span::raw(" "));
            }
            continue;
        }
        let mut style = Style::default();
        if let Some(ref c) = span.color {
            style = style.fg(color_from_name(c));
        }
        if span.bold {
            style = style.bold();
        }
        rendered.push(Span::styled(span.text, style));
    }
    (!rendered.is_empty()).then(|| Line::from(rendered))
}
