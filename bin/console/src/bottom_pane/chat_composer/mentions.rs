use super::*;

impl ChatComposer {
    pub(crate) fn mentions_enabled(&self) -> bool {
        self.connectors_enabled
            && self
                .connectors_snapshot
                .as_ref()
                .is_some_and(|snapshot| !snapshot.connectors.is_empty())
    }

    /// Extract a token prefixed with `prefix` under the cursor, if any.
    ///
    /// The returned string **does not** include the prefix.
    ///
    /// Behavior:
    /// - The cursor may be anywhere *inside* the token (including on the
    ///   leading prefix). It does **not** need to be at the end of the line.
    /// - A token is delimited by ASCII whitespace (space, tab, newline).
    /// - If the cursor is on `prefix` inside an existing token (for example the
    ///   second `@` in `@scope/pkg@latest`), keep treating the surrounding
    ///   whitespace-delimited token as the active token rather than starting a
    ///   new token at that nested prefix.
    /// - If the token under the cursor starts with `prefix`, that token is
    ///   returned without the leading prefix. When `allow_empty` is true, a
    ///   lone prefix character yields `Some(String::new())` to surface hints.
    pub(super) fn current_prefixed_token(
        textarea: &TextArea,
        prefix: char,
        allow_empty: bool,
    ) -> Option<String> {
        let cursor_offset = textarea.cursor();
        let text = textarea.text();

        // Adjust the provided byte offset to the nearest valid char boundary at or before it.
        let mut safe_cursor = cursor_offset.min(text.len());
        // If we're not on a char boundary, move back to the start of the current char.
        if safe_cursor < text.len() && !text.is_char_boundary(safe_cursor) {
            // Find the last valid boundary <= cursor_offset.
            safe_cursor = text
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= cursor_offset)
                .last()
                .unwrap_or(0);
        }

        // Split the line around the (now safe) cursor position.
        let before_cursor = &text[..safe_cursor];
        let after_cursor = &text[safe_cursor..];

        // Detect whether we're on whitespace at the cursor boundary.
        let at_whitespace = if safe_cursor < text.len() {
            text[safe_cursor..]
                .chars()
                .next()
                .map(char::is_whitespace)
                .unwrap_or(false)
        } else {
            false
        };

        // Left candidate: token containing the cursor position.
        let start_left = before_cursor
            .char_indices()
            .rfind(|(_, c)| c.is_whitespace())
            .map(|(idx, c)| idx + c.len_utf8())
            .unwrap_or(0);
        let end_left_rel = after_cursor
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, _)| idx)
            .unwrap_or(after_cursor.len());
        let end_left = safe_cursor + end_left_rel;
        let token_left = if start_left < end_left {
            Some(&text[start_left..end_left])
        } else {
            None
        };

        // Right candidate: token immediately after any whitespace from the cursor.
        let ws_len_right: usize = after_cursor
            .chars()
            .take_while(|c| c.is_whitespace())
            .map(char::len_utf8)
            .sum();
        let start_right = safe_cursor + ws_len_right;
        let end_right_rel = text[start_right..]
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, _)| idx)
            .unwrap_or(text.len() - start_right);
        let end_right = start_right + end_right_rel;
        let token_right = if start_right < end_right {
            Some(&text[start_right..end_right])
        } else {
            None
        };

        let prefix_str = prefix.to_string();
        let left_match = token_left.filter(|t| t.starts_with(prefix));
        let right_match = token_right.filter(|t| t.starts_with(prefix));

        let left_prefixed = left_match.map(|t| t[prefix.len_utf8()..].to_string());
        let right_prefixed = right_match.map(|t| t[prefix.len_utf8()..].to_string());

        if at_whitespace {
            if right_prefixed.is_some() {
                return right_prefixed;
            }
            if token_left.is_some_and(|t| t == prefix_str) {
                return allow_empty.then(String::new);
            }
            return left_prefixed;
        }
        if after_cursor.starts_with(prefix) {
            let prefix_starts_token = before_cursor
                .chars()
                .next_back()
                .is_none_or(char::is_whitespace);
            return if prefix_starts_token {
                right_prefixed.or(left_prefixed)
            } else {
                left_prefixed
            };
        }
        left_prefixed.or(right_prefixed)
    }

    /// Extract the `@token` that the cursor is currently positioned on, if any.
    ///
    /// The returned string **does not** include the leading `@`.
    pub(super) fn current_at_token(textarea: &TextArea) -> Option<String> {
        Self::current_prefixed_token(textarea, '@', /*allow_empty*/ false)
    }

    pub(super) fn current_mention_token(&self) -> Option<String> {
        if !self.mentions_enabled() {
            return None;
        }
        Self::current_prefixed_token(&self.textarea, '$', /*allow_empty*/ true)
    }

    /// Replace the active `@token` (the one under the cursor) with `path`.
    ///
    /// The algorithm mirrors `current_at_token` so replacement works no matter
    /// where the cursor is within the token and regardless of how many
    /// `@tokens` exist in the line.
    pub(super) fn insert_selected_path(&mut self, path: &str) {
        let cursor_offset = self.textarea.cursor();
        let text = self.textarea.text();
        // Clamp to a valid char boundary to avoid panics when slicing.
        let safe_cursor = Self::clamp_to_char_boundary(text, cursor_offset);

        let before_cursor = &text[..safe_cursor];
        let after_cursor = &text[safe_cursor..];

        // Determine token boundaries.
        let start_idx = before_cursor
            .char_indices()
            .rfind(|(_, c)| c.is_whitespace())
            .map(|(idx, c)| idx + c.len_utf8())
            .unwrap_or(0);

        let end_rel_idx = after_cursor
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, _)| idx)
            .unwrap_or(after_cursor.len());
        let end_idx = safe_cursor + end_rel_idx;

        // If the path contains whitespace, wrap it in double quotes so the
        // local prompt arg parser treats it as a single argument. Avoid adding
        // quotes when the path already contains one to keep behavior simple.
        let needs_quotes = path.chars().any(char::is_whitespace);
        let inserted = if needs_quotes && !path.contains('"') {
            format!("\"{path}\"")
        } else {
            path.to_string()
        };

        // Replace just the active `@token` so unrelated text elements, such as
        // large-paste placeholders, remain atomic and can still expand on submit.
        self.textarea
            .replace_range(start_idx..end_idx, &format!("{inserted} "));
        let new_cursor = start_idx.saturating_add(inserted.len()).saturating_add(1);
        self.textarea.set_cursor(new_cursor);
    }

    pub(super) fn mention_name_from_insert_text(insert_text: &str) -> Option<String> {
        let name = insert_text.strip_prefix('$')?;
        if name.is_empty() {
            return None;
        }
        if name
            .as_bytes()
            .iter()
            .all(|byte| is_mention_name_char(*byte))
        {
            Some(name.to_string())
        } else {
            None
        }
    }

    pub(super) fn current_mention_elements(&self) -> Vec<(u64, String)> {
        self.textarea
            .text_element_snapshots()
            .into_iter()
            .filter_map(|snapshot| {
                Self::mention_name_from_insert_text(snapshot.text.as_str())
                    .map(|mention| (snapshot.id, mention))
            })
            .collect()
    }

    pub(super) fn snapshot_mention_bindings(&self) -> Vec<MentionBinding> {
        let mut ordered = Vec::new();
        for (id, mention) in self.current_mention_elements() {
            if let Some(binding) = self.mention_bindings.get(&id)
                && binding.mention == mention
            {
                ordered.push(MentionBinding {
                    mention: binding.mention.clone(),
                    path: binding.path.clone(),
                });
            }
        }
        ordered
    }

    pub(super) fn bind_mentions_from_snapshot(&mut self, mention_bindings: Vec<MentionBinding>) {
        self.mention_bindings.clear();
        if mention_bindings.is_empty() {
            return;
        }

        let text = self.textarea.text().to_string();
        let mut scan_from = 0usize;
        for binding in mention_bindings {
            let token = format!("${}", binding.mention);
            let Some(range) =
                find_next_mention_token_range(text.as_str(), token.as_str(), scan_from)
            else {
                continue;
            };

            let id = if let Some(id) = self.textarea.add_element_range(range.clone()) {
                Some(id)
            } else {
                self.textarea.element_id_for_exact_range(range.clone())
            };

            if let Some(id) = id {
                self.mention_bindings.insert(
                    id,
                    ComposerMentionBinding {
                        mention: binding.mention,
                        path: binding.path,
                    },
                );
                scan_from = range.end;
            }
        }
    }
}

pub(super) fn is_mention_name_char(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-')
}

pub(super) fn find_next_mention_token_range(
    text: &str,
    token: &str,
    from: usize,
) -> Option<Range<usize>> {
    if token.is_empty() || from >= text.len() {
        return None;
    }
    let bytes = text.as_bytes();
    let token_bytes = token.as_bytes();
    let mut index = from;

    while index < bytes.len() {
        if bytes[index] != b'$' {
            index += 1;
            continue;
        }

        let end = index.saturating_add(token_bytes.len());
        if end > bytes.len() {
            return None;
        }
        if &bytes[index..end] != token_bytes {
            index += 1;
            continue;
        }

        if bytes
            .get(end)
            .is_none_or(|byte| !is_mention_name_char(*byte))
        {
            return Some(index..end);
        }

        index = end;
    }

    None
}
