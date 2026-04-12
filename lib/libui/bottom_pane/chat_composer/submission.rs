use super::{
    ActivePopup, ByteRange, ChatComposer, HistoryEntry, InputResult, Instant,
    MAX_USER_INPUT_TEXT_CHARS, PROMPTS_CMD_PREFIX, PromptSelectionAction, PromptSelectionMode,
    SlashCommand, TextElement, expand_custom_prompt, expand_if_numeric_with_positional_args,
    history_cell, parse_slash_name, prompt_argument_names, prompt_command_with_arg_placeholders,
    prompt_has_numeric_placeholders, slash_commands, user_input_too_large_message,
};
use crate::app_event::AppEvent;
use chaos_ipc::custom_prompts::CustomPrompt;

impl ChatComposer {
    /// Prepare text for submission/queuing. Returns None if submission should be suppressed.
    /// On success, clears pending paste payloads because placeholders have been expanded.
    ///
    /// When `record_history` is true, the final submission is stored for Up/Down recall.
    pub(super) fn prepare_submission_text(
        &mut self,
        record_history: bool,
    ) -> Option<(String, Vec<TextElement>)> {
        let mut text = self.textarea.text().to_string();
        let original_input = text.clone();
        let original_text_elements = self.textarea.text_elements();
        let original_mention_bindings = self.snapshot_mention_bindings();
        let original_local_image_paths = self
            .attached_images
            .iter()
            .map(|img| img.path.clone())
            .collect::<Vec<_>>();
        let original_pending_pastes = self.pending_pastes.clone();
        let mut text_elements = original_text_elements.clone();
        let input_starts_with_space = original_input.starts_with(' ');
        self.recent_submission_mention_bindings.clear();
        self.textarea.set_text_clearing_elements("");

        if !self.pending_pastes.is_empty() {
            // Expand placeholders so element byte ranges stay aligned.
            let (expanded, expanded_elements) =
                Self::expand_pending_pastes(&text, text_elements, &self.pending_pastes);
            text = expanded;
            text_elements = expanded_elements;
        }

        let expanded_input = text.clone();

        // If there is neither text nor attachments, suppress submission entirely.
        text = text.trim().to_string();
        text_elements = Self::trim_text_elements(&expanded_input, &text, text_elements);

        if self.slash_commands_enabled()
            && let Some((name, _rest, _rest_offset)) = parse_slash_name(&text)
        {
            let treat_as_plain_text = input_starts_with_space || name.contains('/');
            if !treat_as_plain_text {
                let is_builtin =
                    slash_commands::find_builtin_command(name, self.builtin_command_flags())
                        .is_some();
                let prompt_prefix = format!("{PROMPTS_CMD_PREFIX}:");
                let is_known_prompt = name
                    .strip_prefix(&prompt_prefix)
                    .map(|prompt_name| {
                        self.custom_prompts
                            .iter()
                            .any(|prompt| prompt.name == prompt_name)
                    })
                    .unwrap_or(false);
                if !is_builtin && !is_known_prompt {
                    let message = format!(
                        r#"Unrecognized command '/{name}'. Type "/" for a list of supported commands."#
                    );
                    self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_info_event(message, /*hint*/ None),
                    )));
                    self.set_text_content_with_mention_bindings(
                        original_input.clone(),
                        original_text_elements,
                        original_local_image_paths,
                        original_mention_bindings,
                    );
                    self.pending_pastes.clone_from(&original_pending_pastes);
                    self.textarea.set_cursor(original_input.len());
                    return None;
                }
            }
        }

        if self.slash_commands_enabled() {
            let expanded_prompt =
                match expand_custom_prompt(&text, &text_elements, &self.custom_prompts) {
                    Ok(expanded) => expanded,
                    Err(err) => {
                        self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                            history_cell::new_error_event(err.user_message()),
                        )));
                        self.set_text_content_with_mention_bindings(
                            original_input.clone(),
                            original_text_elements,
                            original_local_image_paths,
                            original_mention_bindings,
                        );
                        self.pending_pastes.clone_from(&original_pending_pastes);
                        self.textarea.set_cursor(original_input.len());
                        return None;
                    }
                };
            if let Some(expanded) = expanded_prompt {
                text = expanded.text;
                text_elements = expanded.text_elements;
            }
        }
        let actual_chars = text.chars().count();
        if actual_chars > MAX_USER_INPUT_TEXT_CHARS {
            let message = user_input_too_large_message(actual_chars);
            self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                history_cell::new_error_event(message),
            )));
            self.set_text_content_with_mention_bindings(
                original_input.clone(),
                original_text_elements,
                original_local_image_paths,
                original_mention_bindings,
            );
            self.pending_pastes.clone_from(&original_pending_pastes);
            self.textarea.set_cursor(original_input.len());
            return None;
        }
        // Custom prompt expansion can remove or rewrite image placeholders, so prune any
        // attachments that no longer have a corresponding placeholder in the expanded text.
        self.prune_attached_images_for_submission(&text, &text_elements);
        if text.is_empty() && self.attached_images.is_empty() && self.remote_image_urls.is_empty() {
            return None;
        }
        self.recent_submission_mention_bindings = original_mention_bindings.clone();
        if record_history
            && (!text.is_empty()
                || !self.attached_images.is_empty()
                || !self.remote_image_urls.is_empty())
        {
            let local_image_paths = self
                .attached_images
                .iter()
                .map(|img| img.path.clone())
                .collect();
            self.history.record_local_submission(HistoryEntry {
                text: text.clone(),
                text_elements: text_elements.clone(),
                local_image_paths,
                remote_image_urls: self.remote_image_urls.clone(),
                mention_bindings: original_mention_bindings,
                pending_pastes: Vec::new(),
            });
        }
        self.pending_pastes.clear();
        Some((text, text_elements))
    }

    /// Common logic for handling message submission/queuing.
    /// Returns the appropriate InputResult based on `should_queue`.
    pub(super) fn handle_submission(&mut self, should_queue: bool) -> (InputResult, bool) {
        self.handle_submission_with_time(should_queue, Instant::now())
    }

    pub(super) fn handle_submission_with_time(
        &mut self,
        should_queue: bool,
        now: Instant,
    ) -> (InputResult, bool) {
        // If the first line is a bare built-in slash command (no args),
        // dispatch it even when the slash popup isn't visible. This preserves
        // the workflow: type a prefix ("/di"), press Tab to complete to
        // "/diff ", then press Enter/Ctrl+Shift+Q to run it. Tab moves the cursor beyond
        // the '/name' token and our caret-based heuristic hides the popup,
        // but Enter/Ctrl+Shift+Q should still dispatch the command rather than submit
        // literal text.
        if let Some(result) = self.try_dispatch_bare_slash_command() {
            return (result, true);
        }

        // If we're in a paste-like burst capture, treat Enter/Ctrl+Shift+Q as part of the burst
        // and accumulate it rather than submitting or inserting immediately.
        // Do not treat as paste inside a slash-command context.
        let in_slash_context = self.slash_commands_enabled()
            && (matches!(self.active_popup, ActivePopup::Command(_))
                || self
                    .textarea
                    .text()
                    .lines()
                    .next()
                    .unwrap_or("")
                    .starts_with('/'));
        if !self.disable_paste_burst
            && self.paste_burst.is_active()
            && !in_slash_context
            && self.paste_burst.append_newline_if_active(now)
        {
            return (InputResult::None, true);
        }

        // During a paste-like burst, treat Enter/Ctrl+Shift+Q as a newline instead of submit.
        if !in_slash_context
            && !self.disable_paste_burst
            && self
                .paste_burst
                .newline_should_insert_instead_of_submit(now)
        {
            self.textarea.insert_str("\n");
            self.paste_burst.extend_window(now);
            return (InputResult::None, true);
        }

        let original_input = self.textarea.text().to_string();
        let original_text_elements = self.textarea.text_elements();
        let original_mention_bindings = self.snapshot_mention_bindings();
        let original_local_image_paths = self
            .attached_images
            .iter()
            .map(|img| img.path.clone())
            .collect::<Vec<_>>();
        let original_pending_pastes = self.pending_pastes.clone();
        if let Some(result) = self.try_dispatch_slash_command_with_args() {
            return (result, true);
        }

        if let Some((text, text_elements)) =
            self.prepare_submission_text(/*record_history*/ true)
        {
            if should_queue {
                (
                    InputResult::Queued {
                        text,
                        text_elements,
                    },
                    true,
                )
            } else {
                // Do not clear attached_images here; ChatWidget drains them via take_recent_submission_images().
                (
                    InputResult::Submitted {
                        text,
                        text_elements,
                    },
                    true,
                )
            }
        } else {
            // Restore text if submission was suppressed.
            self.set_text_content_with_mention_bindings(
                original_input,
                original_text_elements,
                original_local_image_paths,
                original_mention_bindings,
            );
            self.pending_pastes = original_pending_pastes;
            (InputResult::None, true)
        }
    }

    /// Check if the first line is a bare slash command (no args) and dispatch it.
    /// Returns Some(InputResult) if a command was dispatched, None otherwise.
    pub(super) fn try_dispatch_bare_slash_command(&mut self) -> Option<InputResult> {
        if !self.slash_commands_enabled() {
            return None;
        }
        let first_line = self.textarea.text().lines().next().unwrap_or("");
        if let Some((name, rest, _rest_offset)) = parse_slash_name(first_line)
            && rest.is_empty()
            && let Some(cmd) =
                slash_commands::find_builtin_command(name, self.builtin_command_flags())
        {
            if self.reject_slash_command_if_unavailable(cmd) {
                return Some(InputResult::None);
            }
            self.textarea.set_text_clearing_elements("");
            Some(InputResult::Command(cmd))
        } else {
            None
        }
    }

    /// Check if the input is a slash command with args (e.g., /review args) and dispatch it.
    /// Returns Some(InputResult) if a command was dispatched, None otherwise.
    pub(super) fn try_dispatch_slash_command_with_args(&mut self) -> Option<InputResult> {
        if !self.slash_commands_enabled() {
            return None;
        }
        let text = self.textarea.text().to_string();
        if text.starts_with(' ') {
            return None;
        }

        let (name, rest, rest_offset) = parse_slash_name(&text)?;
        if rest.is_empty() || name.contains('/') {
            return None;
        }

        let cmd = slash_commands::find_builtin_command(name, self.builtin_command_flags())?;

        if !cmd.supports_inline_args() {
            return None;
        }
        if self.reject_slash_command_if_unavailable(cmd) {
            return Some(InputResult::None);
        }

        let mut args_elements =
            Self::slash_command_args_elements(rest, rest_offset, &self.textarea.text_elements());
        let trimmed_rest = rest.trim();
        args_elements = Self::trim_text_elements(rest, trimmed_rest, args_elements);
        Some(InputResult::CommandWithArgs(
            cmd,
            trimmed_rest.to_string(),
            args_elements,
        ))
    }

    /// Expand pending placeholders and extract normalized inline-command args.
    ///
    /// Inline-arg commands are initially dispatched using the raw draft so command rejection does
    /// not consume user input. Once a command is accepted, this helper performs the usual
    /// submission preparation (paste expansion, element trimming) and rebases element ranges from
    /// full-text offsets to command-arg offsets.
    pub fn prepare_inline_args_submission(
        &mut self,
        record_history: bool,
    ) -> Option<(String, Vec<TextElement>)> {
        let (prepared_text, prepared_elements) = self.prepare_submission_text(record_history)?;
        let (_, prepared_rest, prepared_rest_offset) = parse_slash_name(&prepared_text)?;
        let mut args_elements = Self::slash_command_args_elements(
            prepared_rest,
            prepared_rest_offset,
            &prepared_elements,
        );
        let trimmed_rest = prepared_rest.trim();
        args_elements = Self::trim_text_elements(prepared_rest, trimmed_rest, args_elements);
        Some((trimmed_rest.to_string(), args_elements))
    }

    pub(super) fn reject_slash_command_if_unavailable(&self, cmd: SlashCommand) -> bool {
        if !self.is_task_running || cmd.available_during_task() {
            return false;
        }
        let message = format!(
            "'/{}' is disabled while a task is in progress.",
            cmd.command()
        );
        self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            history_cell::new_error_event(message),
        )));
        true
    }

    /// Translate full-text element ranges into command-argument ranges.
    ///
    /// `rest_offset` is the byte offset where `rest` begins in the full text.
    pub(super) fn slash_command_args_elements(
        rest: &str,
        rest_offset: usize,
        text_elements: &[TextElement],
    ) -> Vec<TextElement> {
        if rest.is_empty() || text_elements.is_empty() {
            return Vec::new();
        }
        text_elements
            .iter()
            .filter_map(|elem| {
                if elem.byte_range.end <= rest_offset {
                    return None;
                }
                let start = elem.byte_range.start.saturating_sub(rest_offset);
                let mut end = elem.byte_range.end.saturating_sub(rest_offset);
                if start >= rest.len() {
                    return None;
                }
                end = end.min(rest.len());
                (start < end).then_some(elem.map_range(|_| ByteRange { start, end }))
            })
            .collect()
    }

    pub(super) fn trim_text_elements(
        original: &str,
        trimmed: &str,
        elements: Vec<TextElement>,
    ) -> Vec<TextElement> {
        if trimmed.is_empty() || elements.is_empty() {
            return Vec::new();
        }
        let trimmed_start = original.len().saturating_sub(original.trim_start().len());
        let trimmed_end = trimmed_start.saturating_add(trimmed.len());

        elements
            .into_iter()
            .filter_map(|elem| {
                let start = elem.byte_range.start;
                let end = elem.byte_range.end;
                if end <= trimmed_start || start >= trimmed_end {
                    return None;
                }
                let new_start = start.saturating_sub(trimmed_start);
                let new_end = end.saturating_sub(trimmed_start).min(trimmed.len());
                if new_start >= new_end {
                    return None;
                }
                let placeholder = trimmed.get(new_start..new_end).map(str::to_string);
                Some(TextElement::new(
                    ByteRange {
                        start: new_start,
                        end: new_end,
                    },
                    placeholder,
                ))
            })
            .collect()
    }
}

pub(super) fn prompt_selection_action(
    prompt: &CustomPrompt,
    first_line: &str,
    mode: PromptSelectionMode,
    text_elements: &[TextElement],
) -> PromptSelectionAction {
    let named_args = prompt_argument_names(&prompt.content);
    let has_numeric = prompt_has_numeric_placeholders(&prompt.content);

    match mode {
        PromptSelectionMode::Completion => {
            if !named_args.is_empty() {
                let (text, cursor) =
                    prompt_command_with_arg_placeholders(&prompt.name, &named_args);
                return PromptSelectionAction::Insert {
                    text,
                    cursor: Some(cursor),
                };
            }
            if has_numeric {
                let text = format!("/{PROMPTS_CMD_PREFIX}:{} ", prompt.name);
                return PromptSelectionAction::Insert { text, cursor: None };
            }
            let text = format!("/{PROMPTS_CMD_PREFIX}:{}", prompt.name);
            PromptSelectionAction::Insert { text, cursor: None }
        }
        PromptSelectionMode::Submit => {
            if !named_args.is_empty() {
                let (text, cursor) =
                    prompt_command_with_arg_placeholders(&prompt.name, &named_args);
                return PromptSelectionAction::Insert {
                    text,
                    cursor: Some(cursor),
                };
            }
            if has_numeric {
                if let Some(expanded) =
                    expand_if_numeric_with_positional_args(prompt, first_line, text_elements)
                {
                    return PromptSelectionAction::Submit {
                        text: expanded.text,
                        text_elements: expanded.text_elements,
                    };
                }
                let text = format!("/{PROMPTS_CMD_PREFIX}:{} ", prompt.name);
                return PromptSelectionAction::Insert { text, cursor: None };
            }
            PromptSelectionAction::Submit {
                text: prompt.content.clone(),
                // By now we know this custom prompt has no args, so no text elements to preserve.
                text_elements: Vec::new(),
            }
        }
    }
}
