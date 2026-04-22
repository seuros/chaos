//! `ChaosWindow` impl: update dispatch, view rendering, and all event helpers.

use chaos_chassis::reducer::NoticeLevel;
use chaos_chassis::theme::ThemeFamily;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::Op;
use iced::Border;
use iced::Element;
use iced::Font;
use iced::Length;
use iced::Theme;
use iced::widget::button;
use iced::widget::column;
use iced::widget::container;
use iced::widget::row;
use iced::widget::scrollable;
use iced::widget::text;
use iced::widget::text_input;

use crate::Message;
use crate::chat::ChatEntry;
use crate::state::ChaosWindow;
use crate::state::Status;
use crate::state::TurnState;
use crate::theme::ChaosPalette;
use crate::theme::button_ghost;
use crate::theme::button_primary;
use crate::theme::container_code;
use crate::theme::container_root;
use crate::theme::container_transcript;
use crate::theme::container_user;
use crate::theme::palette_for_family;

impl ChaosWindow {
    /// Active palette. [`PHOSPHOR`] unless the GUI has been clamped to
    /// Claude Code MAX, in which case [`ANTHROPIC`] takes over.
    pub fn palette(&self) -> ChaosPalette {
        palette_for_family(self.theme_family())
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

    fn theme_family(&self) -> ThemeFamily {
        if self.clamped {
            ThemeFamily::Anthropic
        } else {
            ThemeFamily::Phosphor
        }
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
                if self.frontend.status != Status::Shutdown {
                    self.mark_kernel_gone();
                } else {
                    self.frontend.turn = TurnState::Idle;
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
        self.frontend.record_user_submission(prompt);
    }

    pub(super) fn handle_kernel_event(&mut self, event: Event) {
        self.frontend.apply_event(event);
    }

    pub(super) fn mark_kernel_gone(&mut self) {
        self.frontend.mark_kernel_gone();
    }

    pub(super) fn can_submit(&self) -> bool {
        self.frontend.can_submit()
    }

    #[cfg(test)]
    pub(super) fn pending_stream_count(&self) -> usize {
        self.frontend.pending_stream_count()
    }

    #[cfg(test)]
    pub(super) fn pending_call_count(&self) -> usize {
        self.frontend.pending_call_count()
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
            self.frontend
                .transcript
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
                .on_press_maybe(
                    (self.frontend.turn == TurnState::InFlight).then_some(Message::Interrupt),
                )
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
    fn render_entry<'a>(&self, entry: &'a ChatEntry, _md_theme: &Theme) -> Element<'a, Message> {
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
            ChatEntry::Agent { content } => column![
                text("chaos").size(12).color(palette.highlight),
                text(content.as_str()).size(14).color(palette.fg),
            ]
            .spacing(4)
            .into(),
            ChatEntry::Reasoning { content } => column![
                text("reasoning").size(12).color(palette.dim),
                text(content.as_str()).size(14).color(palette.dim),
            ]
            .spacing(4)
            .into(),
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
            status = self.frontend.status,
            turn = self.frontend.turn,
        );
        if let Some(usage) = &self.frontend.token_usage {
            format!(
                "{base} — tokens in:{} out:{} total:{}",
                usage.input_tokens, usage.output_tokens, usage.total_tokens,
            )
        } else {
            base
        }
    }
}
