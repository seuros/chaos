//! `ChaosWindow` impl: update dispatch, view rendering, and all event helpers.

use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ExecCommandBeginEvent;
use chaos_ipc::protocol::ExecCommandEndEvent;
use chaos_ipc::protocol::McpToolCallBeginEvent;
use chaos_ipc::protocol::McpToolCallEndEvent;
use chaos_ipc::protocol::Op;
use iced::Border;
use iced::Element;
use iced::Font;
use iced::Length;
use iced::Theme;
use iced::widget::button;
use iced::widget::column;
use iced::widget::container;
use iced::widget::markdown;
use iced::widget::row;
use iced::widget::scrollable;
use iced::widget::text;
use iced::widget::text_input;

use crate::Message;
use crate::chat::ChatEntry;
use crate::chat::MatchAgent;
use crate::chat::MatchReasoning;
use crate::chat::NoticeLevel;
use crate::chat::StreamMatch;
use crate::format_error;
use crate::state::ChaosWindow;
use crate::state::Status;
use crate::state::TurnState;
use crate::theme::ANTHROPIC;
use crate::theme::ChaosPalette;
use crate::theme::PHOSPHOR;
use crate::theme::button_ghost;
use crate::theme::button_primary;
use crate::theme::container_code;
use crate::theme::container_root;
use crate::theme::container_transcript;
use crate::theme::container_user;

impl ChaosWindow {
    /// Active palette. [`PHOSPHOR`] unless the GUI has been clamped to
    /// Claude Code MAX, in which case [`ANTHROPIC`] takes over.
    pub fn palette(&self) -> ChaosPalette {
        if self.clamped { ANTHROPIC } else { PHOSPHOR }
    }

    /// Active iced [`Theme`] derived from the current palette. Called by
    /// iced's application-level theme hook on every frame and by
    /// `render_entry` whenever it needs to hand a `Settings` into the
    /// markdown viewer.
    pub fn theme(&self) -> Theme {
        let name = if self.clamped {
            "chaos-anthropic"
        } else {
            "chaos-phosphor"
        };
        self.palette().to_theme(name)
    }

    /// Handle a single message.
    pub fn update(&mut self, message: Message) {
        match message {
            Message::ComposerChanged(text) => {
                self.composer = text;
            }
            Message::ComposerSubmit => self.submit_turn(),
            Message::ToggleClamped => {
                self.clamped = !self.clamped;
            }
            Message::Interrupt => {
                // Submit the interrupt but *do not* release `InFlight` —
                // the kernel will emit `TurnAborted` (or `Error`) once the
                // in-progress turn actually unwinds, and the composer stays
                // gated until that terminal event lands.
                if self.op_tx.send(Op::Interrupt).is_err() {
                    self.mark_kernel_gone();
                }
            }
            Message::KernelEvent(event) => self.handle_kernel_event(*event),
            Message::Nop => {}
            Message::KernelDisconnected => {
                // Only upgrade to Shutdown if we haven't already seen
                // `ShutdownComplete` — otherwise the transcript would
                // double-log. Always unblock the composer: there is no
                // kernel to wait on.
                if self.status != Status::Shutdown {
                    self.mark_kernel_gone();
                } else {
                    self.turn = TurnState::Idle;
                }
            }
        }
    }

    pub(super) fn submit_turn(&mut self) {
        if !self.can_submit() {
            return;
        }
        // Trim first, take later — a whitespace-only composer should leave
        // the user's text alone so they can keep editing without losing it.
        let trimmed = self.composer.trim();
        if trimmed.is_empty() {
            return;
        }
        let prompt = trimmed.to_string();
        let op = self.template.build_turn(prompt.clone());
        if self.op_tx.send(op).is_err() {
            self.mark_kernel_gone();
            return;
        }
        self.composer.clear();
        self.transcript.push(ChatEntry::User { text: prompt });
        self.turn = TurnState::InFlight;
    }

    pub(super) fn handle_kernel_event(&mut self, event: Event) {
        match event.msg {
            // ---- Session lifecycle -----------------------------------------
            EventMsg::SessionConfigured(_) => {
                self.status = Status::Ready;
            }
            EventMsg::ShutdownComplete => {
                self.status = Status::Shutdown;
                self.turn = TurnState::Idle;
                self.clear_pending_bookkeeping();
            }

            // ---- Terminal turn events release the composer ----------------
            EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_) => {
                self.turn = TurnState::Idle;
                // A new turn starts fresh — any half-streamed deltas and
                // unclosed exec/tool calls are orphaned by the kernel now
                // that the turn has concluded. Dropping them here prevents
                // a recycled `call_id` from mutating stale transcript
                // slots on the next turn.
                self.clear_pending_bookkeeping();
            }

            // ---- Agent message: streaming + finalized --------------------
            EventMsg::AgentMessageContentDelta(delta) => {
                self.push_agent_delta(delta.item_id, &delta.delta);
            }
            EventMsg::AgentMessage(msg) => {
                // Finalize: reparse from scratch so the rendered markdown
                // matches exactly what the kernel emitted, even if we
                // already streamed a partial version.
                self.finalize_agent_message(&msg.message);
            }

            // ---- Reasoning: streaming + finalized ------------------------
            EventMsg::ReasoningContentDelta(delta) => {
                self.push_reasoning_delta(delta.item_id, &delta.delta);
            }
            EventMsg::AgentReasoning(reasoning) => {
                self.finalize_reasoning(&reasoning.text);
            }

            // ---- Exec commands: begin / end pairs ------------------------
            EventMsg::ExecCommandBegin(begin) => self.push_exec_begin(begin),
            EventMsg::ExecCommandEnd(end) => self.complete_exec(end),

            // ---- MCP tool calls: begin / end pairs -----------------------
            EventMsg::McpToolCallBegin(begin) => self.push_tool_begin(begin),
            EventMsg::McpToolCallEnd(end) => self.complete_tool(end),

            // ---- Errors with proper structure ----------------------------
            EventMsg::Error(err) => {
                let text = format_error(&err.message, err.chaos_error_info.as_ref());
                self.transcript.push(ChatEntry::Notice {
                    level: NoticeLevel::Error,
                    text,
                });
                self.turn = TurnState::Idle;
                self.clear_pending_bookkeeping();
            }
            EventMsg::StreamError(err) => {
                // StreamError is non-fatal: the kernel is probably retrying.
                // Keep InFlight held so the composer stays gated until the
                // real terminal event lands.
                let text = format_error(&err.message, err.chaos_error_info.as_ref());
                self.transcript.push(ChatEntry::Notice {
                    level: NoticeLevel::Warn,
                    text: format!("stream hiccup: {text}"),
                });
            }
            EventMsg::Warning(warn) => {
                self.transcript.push(ChatEntry::Notice {
                    level: NoticeLevel::Warn,
                    text: warn.message,
                });
            }
            EventMsg::BackgroundEvent(bg) => {
                self.transcript.push(ChatEntry::Notice {
                    level: NoticeLevel::Info,
                    text: bg.message,
                });
            }
            EventMsg::DeprecationNotice(notice) => {
                self.transcript.push(ChatEntry::Notice {
                    level: NoticeLevel::Warn,
                    text: format!("deprecated: {notice:?}"),
                });
            }

            // ---- Token usage feeds the header, not the transcript -------
            EventMsg::TokenCount(tc) => {
                if let Some(info) = tc.info {
                    self.token_usage = Some(info.total_token_usage);
                }
            }

            // ---- Long tail: intentionally unrendered for now -------------
            //
            // #15 covers the variants that carry real user-visible content;
            // the rest (raw response items, list-tools responses, collab
            // interactions, plan deltas, approval requests, …) either need
            // dedicated UI in #16/#17-follow-ups or are not meaningful
            // standalone. They are not terminal, so `InFlight` stays held.
            _ => {}
        }
    }

    /// Push or extend a streaming agent message keyed by `item_id`.
    fn push_agent_delta(&mut self, item_id: String, delta: &str) {
        if let Some(idx) = self.pending_streams.get(&item_id).copied() {
            if let Some(ChatEntry::Agent { content }) = self.transcript.get_mut(idx) {
                content.push_str(delta);
                return;
            }
            // Stale index (shouldn't happen, but don't corrupt the stream).
            self.pending_streams.remove(&item_id);
        }
        let idx = self.transcript.len();
        self.transcript.push(ChatEntry::Agent {
            content: markdown::Content::parse(delta),
        });
        self.pending_streams.insert(item_id, idx);
    }

    /// Finalize an agent message — reparses the full text so the rendered
    /// markdown can never desync from the kernel's authoritative output.
    ///
    /// `AgentMessageEvent` does not carry an `item_id`, so we can't
    /// match it back to a specific streaming entry. Walk the transcript
    /// backwards from the tail, bounded by the most recent `User` entry
    /// (the natural boundary between the current turn and the previous
    /// one), and finalize the last `Agent` entry we find. This is robust
    /// against `TurnComplete` already having cleared `pending_streams`,
    /// which would otherwise cause a late finalize to append a duplicate.
    ///
    /// Known limitation: if the kernel interleaves two agent streams in
    /// the same turn, this finalizes only the most recent one — the
    /// other remains as a streamed partial. The scaffold assumes one
    /// agent stream per turn, which matches the current protocol.
    fn finalize_agent_message(&mut self, full: &str) {
        if self.finalize_in_place::<MatchAgent>(full) {
            return;
        }
        self.transcript.push(ChatEntry::Agent {
            content: markdown::Content::parse(full),
        });
    }

    fn push_reasoning_delta(&mut self, item_id: String, delta: &str) {
        if let Some(idx) = self.pending_streams.get(&item_id).copied() {
            if let Some(ChatEntry::Reasoning { content }) = self.transcript.get_mut(idx) {
                content.push_str(delta);
                return;
            }
            self.pending_streams.remove(&item_id);
        }
        let idx = self.transcript.len();
        self.transcript.push(ChatEntry::Reasoning {
            content: markdown::Content::parse(delta),
        });
        self.pending_streams.insert(item_id, idx);
    }

    fn finalize_reasoning(&mut self, full: &str) {
        if self.finalize_in_place::<MatchReasoning>(full) {
            return;
        }
        self.transcript.push(ChatEntry::Reasoning {
            content: markdown::Content::parse(full),
        });
    }

    /// Tail-scan the transcript for the most recent entry matching `M`,
    /// stopping at the turn boundary (last `User` entry). On hit, replace
    /// the entry in place with a fresh finalized copy of `full`, drop any
    /// stale `pending_streams` index pointing at the overwritten slot, and
    /// return `true`. On miss, return `false` so the caller can push a
    /// fresh entry.
    fn finalize_in_place<M: StreamMatch>(&mut self, full: &str) -> bool {
        for idx in (0..self.transcript.len()).rev() {
            match &self.transcript[idx] {
                ChatEntry::User { .. } => return false,
                entry if M::matches(entry) => {
                    self.transcript[idx] = M::rebuild(full);
                    self.pending_streams.retain(|_, v| *v != idx);
                    return true;
                }
                _ => continue,
            }
        }
        false
    }

    fn push_exec_begin(&mut self, begin: ExecCommandBeginEvent) {
        let idx = self.transcript.len();
        self.transcript.push(ChatEntry::Exec {
            command: begin.command,
            cwd: begin.cwd,
            exit_code: None,
            output: String::new(),
        });
        self.pending_calls.insert(begin.call_id, idx);
    }

    fn complete_exec(&mut self, end: ExecCommandEndEvent) {
        // Prefer the aggregated output (matches what the agent saw); fall
        // back to stdout then stderr so the GUI never renders a blank
        // shell block.
        let preview = if !end.aggregated_output.is_empty() {
            end.aggregated_output
        } else if !end.stdout.is_empty() {
            end.stdout
        } else {
            end.stderr
        };
        if let Some(idx) = self.pending_calls.remove(&end.call_id)
            && let Some(ChatEntry::Exec {
                exit_code, output, ..
            }) = self.transcript.get_mut(idx)
        {
            *exit_code = Some(end.exit_code);
            *output = preview;
            return;
        }
        // Orphan end with no matching begin — render it as a standalone
        // finished exec entry so nothing gets silently dropped.
        self.transcript.push(ChatEntry::Exec {
            command: end.command,
            cwd: end.cwd,
            exit_code: Some(end.exit_code),
            output: preview,
        });
    }

    fn push_tool_begin(&mut self, begin: McpToolCallBeginEvent) {
        let idx = self.transcript.len();
        self.transcript.push(ChatEntry::Tool {
            server: begin.invocation.server,
            tool: begin.invocation.tool,
            result: None,
        });
        self.pending_calls.insert(begin.call_id, idx);
    }

    fn complete_tool(&mut self, end: McpToolCallEndEvent) {
        let outcome: Result<String, String> = match &end.result {
            Ok(_) => Ok(format!("ok in {:?}", end.duration)),
            Err(err) => Err(err.clone()),
        };
        if let Some(idx) = self.pending_calls.remove(&end.call_id)
            && let Some(ChatEntry::Tool { result, .. }) = self.transcript.get_mut(idx)
        {
            *result = Some(outcome);
            return;
        }
        self.transcript.push(ChatEntry::Tool {
            server: end.invocation.server,
            tool: end.invocation.tool,
            result: Some(outcome),
        });
    }

    pub(super) fn mark_kernel_gone(&mut self) {
        self.status = Status::Shutdown;
        self.turn = TurnState::Idle;
        self.clear_pending_bookkeeping();
        self.transcript.push(ChatEntry::Notice {
            level: NoticeLevel::Error,
            text: "op_tx closed — kernel is gone".to_string(),
        });
    }

    /// Drop any streaming / call-pairing bookkeeping. Called on every
    /// terminal turn event and on session death so stale indices can't
    /// leak across turns (a recycled `call_id` or `item_id` could
    /// otherwise mutate a slot that now belongs to a different entry).
    pub(super) fn clear_pending_bookkeeping(&mut self) {
        self.pending_streams.clear();
        self.pending_calls.clear();
    }

    pub(super) fn can_submit(&self) -> bool {
        self.status == Status::Ready && self.turn == TurnState::Idle
    }

    /// Render the current state as an iced widget tree.
    pub fn view(&self) -> Element<'_, Message> {
        let palette = self.palette();
        // Build the theme once per frame instead of once per agent/reasoning
        // entry — `Theme::Custom` wraps an `Arc<Custom>` so cloning is cheap,
        // but `markdown::view` accepts `&Theme` via `Settings: From<&Theme>`
        // so we can hand out references and skip the clones entirely.
        let md_theme = self.theme();

        // Header: one-line status tinted with the highlight color. The text
        // widget's `.color()` short-circuits around `Theme::Catalog` so the
        // color applies regardless of how built-in iced themes render text.
        let header = text(self.header_text()).size(18).color(palette.highlight);

        let transcript = column(
            self.transcript
                .iter()
                .map(|e| self.render_entry(e, &md_theme)),
        )
        .spacing(12)
        .padding(4);

        let theme_label = if self.clamped { "Phosphor" } else { "Clamp" };

        let composer_row = row![
            text_input("Ask chaos…", &self.composer)
                .on_input(Message::ComposerChanged)
                .on_submit(Message::ComposerSubmit)
                .padding(8),
            button("Send")
                .on_press_maybe(self.can_submit().then_some(Message::ComposerSubmit))
                .style(button_primary(palette)),
            button("Interrupt")
                .on_press_maybe((self.turn == TurnState::InFlight).then_some(Message::Interrupt),)
                .style(button_ghost(palette)),
            button(text(theme_label))
                .on_press(Message::ToggleClamped)
                .style(button_ghost(palette)),
        ]
        .spacing(8);

        let transcript_container = container(scrollable(transcript))
            .height(Length::Fill)
            .padding(8)
            .style(move |_theme: &Theme| container_transcript(palette));

        let body = column![header, transcript_container, composer_row]
            .spacing(12)
            .padding(16);

        // Root container paints the background color; without this iced
        // would render the window with its default theme background instead
        // of the phosphor / anthropic base color.
        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_theme: &Theme| container_root(palette))
            .into()
    }

    /// Render a single transcript entry. Each arm picks the widget that
    /// makes sense for the content and applies palette-driven colors.
    ///
    /// Free function before the theme pass; converted to a method so it
    /// can read `self.palette()` without threading a parameter through
    /// every call site. `md_theme` is borrowed from `view()` so the
    /// Arc-backed `Theme::Custom` isn't rebuilt per entry. Returned
    /// [`Element`] borrows only from `entry` — palette colors are
    /// [`Color`] (Copy) and `markdown::view` borrows the theme, so
    /// nothing borrows from `self`.
    fn render_entry<'a>(&self, entry: &'a ChatEntry, md_theme: &Theme) -> Element<'a, Message> {
        let palette = self.palette();
        match entry {
            ChatEntry::User { text: body } => {
                // `text(&str)` builds `Text<'a>` that borrows from the
                // entry — no allocation, no clone per frame.
                let bubble = container(
                    column![
                        text("you").size(12).color(palette.accent),
                        text(body.as_str()).size(14).color(palette.fg),
                    ]
                    .spacing(2),
                )
                .padding(6)
                .style(move |_theme: &Theme| container_user(palette));
                bubble.into()
            }
            ChatEntry::Agent { content } => {
                let md = markdown::view(content.items(), md_theme);
                column![
                    text("chaos").size(12).color(palette.highlight),
                    md.map(|_uri| Message::Nop),
                ]
                .spacing(4)
                .into()
            }
            ChatEntry::Reasoning { content } => {
                let md = markdown::view(content.items(), md_theme);
                column![
                    text("reasoning").size(12).color(palette.dim),
                    md.map(|_uri| Message::Nop),
                ]
                .spacing(4)
                .into()
            }
            ChatEntry::Exec {
                command,
                cwd,
                exit_code,
                output,
            } => {
                let cmdline = command.join(" ");
                let (status_label, status_color) = match exit_code {
                    Some(0) => ("done".to_string(), palette.success),
                    Some(code) => (format!("exit {code}"), palette.error),
                    None => ("running…".to_string(), palette.warning),
                };
                let header_line = row![
                    text(format!("$ {cmdline}"))
                        .size(13)
                        .font(Font::MONOSPACE)
                        .color(palette.highlight),
                    text(format!("  [{}]", cwd.display()))
                        .size(12)
                        .color(palette.dim),
                    text(format!(" ({status_label})"))
                        .size(12)
                        .color(status_color),
                ]
                .spacing(0);
                let mut col = column![header_line].spacing(4);
                if !output.is_empty() {
                    // Truncate the preview so a 50MB build log doesn't wedge
                    // the GUI. The transcript entry keeps the full `output`
                    // string for future "expand" UI.
                    let preview: String = output.chars().take(1_000).collect();
                    col = col.push(
                        text(preview)
                            .size(12)
                            .font(Font::MONOSPACE)
                            .color(palette.fg),
                    );
                }
                container(col)
                    .padding(6)
                    .style(move |_theme: &Theme| container_code(palette))
                    .into()
            }
            ChatEntry::Tool {
                server,
                tool,
                result,
            } => {
                let label = format!("tool: {server}/{tool}");
                // Discriminate on the structural `Result`, not on string
                // prefix: success → success color, failure → error color,
                // running → warning color. Storing `Result<String,String>`
                // instead of a raw string removed the stringly-typed path.
                let (status_text, status_color) = match result {
                    Some(Ok(r)) => (r.as_str(), palette.success),
                    Some(Err(r)) => (r.as_str(), palette.error),
                    None => ("running…", palette.warning),
                };
                let inner = column![
                    text(label)
                        .size(13)
                        .font(Font::MONOSPACE)
                        .color(palette.accent),
                    text(status_text).size(12).color(status_color),
                ]
                .spacing(4);
                container(inner)
                    .padding(6)
                    .style(move |_theme: &Theme| container_code(palette))
                    .into()
            }
            ChatEntry::Notice { level, text: body } => {
                let (tag, color) = match level {
                    NoticeLevel::Info => ("info", palette.dim),
                    NoticeLevel::Warn => ("warn", palette.warning),
                    NoticeLevel::Error => ("error", palette.error),
                };
                // Wrap the notice text in a padded container so it lines up
                // visually with the other padded/bordered entry arms. Border
                // color follows the severity.
                let inner = text(format!("[{tag}] {body}")).size(13).color(color);
                container(inner)
                    .padding(6)
                    .style(move |_theme: &Theme| container::Style {
                        background: None,
                        text_color: Some(color),
                        border: Border {
                            color,
                            width: 1.0,
                            radius: 2.0.into(),
                        },
                        ..container::Style::default()
                    })
                    .into()
            }
        }
    }

    /// Compact one-line status for the header: session state, turn state,
    /// and (if known) token usage.
    pub(super) fn header_text(&self) -> String {
        let base = format!(
            "chaos-xclient — {status:?} — {turn:?}",
            status = self.status,
            turn = self.turn,
        );
        if let Some(usage) = &self.token_usage {
            format!(
                "{base} — tokens in:{} out:{} total:{}",
                usage.input_tokens, usage.output_tokens, usage.total_tokens,
            )
        } else {
            base
        }
    }
}
