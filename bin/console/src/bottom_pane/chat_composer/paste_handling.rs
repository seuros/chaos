use super::*;

use crate::bottom_pane::LocalImageAttachment;
use crate::clipboard_paste::normalize_pasted_path;
use crate::clipboard_paste::pasted_image_format;
use chaos_ipc::models::local_image_label_text;
use chaos_ipc::user_input::ByteRange;
use chaos_ipc::user_input::TextElement;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::PathBuf;

impl ChatComposer {
    /// Integrate pasted text into the composer.
    ///
    /// Acts as the only place where paste text is integrated, both for:
    ///
    /// - Real/explicit paste events surfaced by the terminal, and
    /// - Non-bracketed "paste bursts" that [`PasteBurst`](super::paste_burst::PasteBurst) buffers
    ///   and later flushes here.
    ///
    /// Behavior:
    ///
    /// - If the paste is larger than `LARGE_PASTE_CHAR_THRESHOLD` chars, inserts a placeholder
    ///   element (expanded on submit) and stores the full text in `pending_pastes`.
    /// - Otherwise, if the paste looks like an image path, attaches the image and inserts a
    ///   trailing space so the user can keep typing naturally.
    /// - Otherwise, inserts the pasted text directly into the textarea.
    ///
    /// In all cases, clears any paste-burst Enter suppression state so a real paste cannot affect
    /// the next user Enter key, then syncs popup state.
    pub fn handle_paste(&mut self, pasted: String) -> bool {
        let pasted = pasted.replace("\r\n", "\n").replace('\r', "\n");
        let char_count = pasted.chars().count();
        if char_count > LARGE_PASTE_CHAR_THRESHOLD {
            let placeholder = self.next_large_paste_placeholder(char_count);
            self.textarea.insert_element(&placeholder);
            self.pending_pastes.push((placeholder, pasted));
        } else if char_count > 1
            && self.image_paste_enabled()
            && self.handle_paste_image_path(pasted.clone())
        {
            self.textarea.insert_str(" ");
        } else {
            self.insert_str(&pasted);
        }
        self.paste_burst.clear_after_explicit_paste();
        self.sync_popups();
        true
    }

    pub fn handle_paste_image_path(&mut self, pasted: String) -> bool {
        let Some(path_buf) = normalize_pasted_path(&pasted) else {
            return false;
        };

        // normalize_pasted_path already handles Windows → WSL path conversion,
        // so we can directly try to read the image dimensions.
        match image::image_dimensions(&path_buf) {
            Ok((width, height)) => {
                tracing::info!("OK: {pasted}");
                tracing::debug!("image dimensions={}x{}", width, height);
                let format = pasted_image_format(&path_buf);
                tracing::debug!("attached image format={}", format.label());
                self.attach_image(path_buf);
                true
            }
            Err(err) => {
                tracing::trace!("ERR: {err}");
                false
            }
        }
    }

    pub(super) fn next_large_paste_placeholder(&mut self, char_count: usize) -> String {
        let base = format!("[Pasted Content {char_count} chars]");
        let next_suffix = self.large_paste_counters.entry(char_count).or_insert(0);
        *next_suffix += 1;
        if *next_suffix == 1 {
            base
        } else {
            format!("{base} #{next_suffix}")
        }
    }

    pub(super) fn is_image_path(path: &str) -> bool {
        let lower = path.to_ascii_lowercase();
        lower.ends_with(".png")
            || lower.ends_with(".jpg")
            || lower.ends_with(".jpeg")
            || lower.ends_with(".gif")
            || lower.ends_with(".webp")
    }

    /// Insert an attachment placeholder and track it for the next submission.
    pub fn attach_image(&mut self, path: PathBuf) {
        let image_number = self.remote_image_urls.len() + self.attached_images.len() + 1;
        let placeholder = local_image_label_text(image_number);
        // Insert as an element to match large paste placeholder behavior:
        // styled distinctly and treated atomically for cursor/mutations.
        self.textarea.insert_element(&placeholder);
        self.attached_images
            .push(AttachedImage { placeholder, path });
    }

    #[cfg(test)]
    pub fn take_recent_submission_images(&mut self) -> Vec<PathBuf> {
        let images = std::mem::take(&mut self.attached_images);
        images.into_iter().map(|img| img.path).collect()
    }

    pub fn take_recent_submission_images_with_placeholders(&mut self) -> Vec<LocalImageAttachment> {
        let images = std::mem::take(&mut self.attached_images);
        images
            .into_iter()
            .map(|img| LocalImageAttachment {
                placeholder: img.placeholder,
                path: img.path,
            })
            .collect()
    }

    pub(super) fn prune_attached_images_for_submission(
        &mut self,
        text: &str,
        text_elements: &[TextElement],
    ) {
        if self.attached_images.is_empty() {
            return;
        }
        let image_placeholders: HashSet<&str> = text_elements
            .iter()
            .filter_map(|elem| elem.placeholder(text))
            .collect();
        self.attached_images
            .retain(|img| image_placeholders.contains(img.placeholder.as_str()));
    }

    pub(super) fn reconcile_deleted_elements(&mut self, elements_before: Vec<String>) {
        let elements_after: HashSet<String> =
            self.textarea.element_payloads().into_iter().collect();

        let mut removed_any_image = false;
        for removed in elements_before
            .into_iter()
            .filter(|payload| !elements_after.contains(payload))
        {
            self.pending_pastes.retain(|(ph, _)| ph != &removed);

            if let Some(idx) = self
                .attached_images
                .iter()
                .position(|img| img.placeholder == removed)
            {
                self.attached_images.remove(idx);
                removed_any_image = true;
            }
        }

        if removed_any_image {
            self.relabel_attached_images_and_update_placeholders();
        }
    }

    pub(super) fn relabel_attached_images_and_update_placeholders(&mut self) {
        for idx in 0..self.attached_images.len() {
            let expected = local_image_label_text(self.remote_image_urls.len() + idx + 1);
            let current = self.attached_images[idx].placeholder.clone();
            if current == expected {
                continue;
            }

            self.attached_images[idx].placeholder = expected.clone();
            let _renamed = self.textarea.replace_element_payload(&current, &expected);
        }
    }

    /// Replace the composer content with text from an external editor.
    /// Clears pending paste placeholders and keeps only attachments whose
    /// placeholder labels still appear in the new text. Image placeholders
    /// are renumbered to `[Image #M+1]..[Image #N]` (where `M` is the number of
    /// remote images). Cursor is placed at the end after rebuilding elements.
    pub(crate) fn apply_external_edit(&mut self, text: String) {
        self.pending_pastes.clear();

        // Count placeholder occurrences in the new text.
        let mut placeholder_counts: HashMap<String, usize> = HashMap::new();
        for placeholder in self.attached_images.iter().map(|img| &img.placeholder) {
            if placeholder_counts.contains_key(placeholder) {
                continue;
            }
            let count = text.match_indices(placeholder).count();
            if count > 0 {
                placeholder_counts.insert(placeholder.clone(), count);
            }
        }

        // Keep attachments only while we have matching occurrences left.
        let mut kept_images = Vec::new();
        for img in self.attached_images.drain(..) {
            if let Some(count) = placeholder_counts.get_mut(&img.placeholder)
                && *count > 0
            {
                *count -= 1;
                kept_images.push(img);
            }
        }
        self.attached_images = kept_images;

        // Rebuild textarea so placeholders become elements again.
        self.textarea.set_text_clearing_elements("");
        let mut remaining: HashMap<&str, usize> = HashMap::new();
        for img in &self.attached_images {
            *remaining.entry(img.placeholder.as_str()).or_insert(0) += 1;
        }

        let mut occurrences: Vec<(usize, &str)> = Vec::new();
        for placeholder in remaining.keys() {
            for (pos, _) in text.match_indices(placeholder) {
                occurrences.push((pos, *placeholder));
            }
        }
        occurrences.sort_unstable_by_key(|(pos, _)| *pos);

        let mut idx = 0usize;
        for (pos, ph) in occurrences {
            let Some(count) = remaining.get_mut(ph) else {
                continue;
            };
            if *count == 0 {
                continue;
            }
            if pos > idx {
                self.textarea.insert_str(&text[idx..pos]);
            }
            self.textarea.insert_element(ph);
            *count -= 1;
            idx = pos + ph.len();
        }
        if idx < text.len() {
            self.textarea.insert_str(&text[idx..]);
        }

        // Keep local image placeholders normalized in attachment order after the
        // remote-image prefix.
        self.relabel_attached_images_and_update_placeholders();
        self.textarea.set_cursor(self.textarea.text().len());
        self.sync_popups();
    }

    pub(crate) fn current_text_with_pending(&self) -> String {
        let mut text = self.textarea.text().to_string();
        for (placeholder, actual) in &self.pending_pastes {
            if text.contains(placeholder) {
                text = text.replace(placeholder, actual);
            }
        }
        text
    }

    pub(crate) fn pending_pastes(&self) -> Vec<(String, String)> {
        self.pending_pastes.clone()
    }

    pub(crate) fn set_pending_pastes(&mut self, pending_pastes: Vec<(String, String)>) {
        let text = self.textarea.text().to_string();
        self.pending_pastes = pending_pastes
            .into_iter()
            .filter(|(placeholder, _)| text.contains(placeholder))
            .collect();
    }

    pub(crate) fn expand_pending_pastes(
        text: &str,
        mut elements: Vec<TextElement>,
        pending_pastes: &[(String, String)],
    ) -> (String, Vec<TextElement>) {
        if pending_pastes.is_empty() || elements.is_empty() {
            return (text.to_string(), elements);
        }

        // Stage 1: index pending paste payloads by placeholder for deterministic replacements.
        let mut pending_by_placeholder: HashMap<&str, VecDeque<&str>> = HashMap::new();
        for (placeholder, actual) in pending_pastes {
            pending_by_placeholder
                .entry(placeholder.as_str())
                .or_default()
                .push_back(actual.as_str());
        }

        // Stage 2: walk elements in order and rebuild text/spans in a single pass.
        elements.sort_by_key(|elem| elem.byte_range.start);

        let mut rebuilt = String::with_capacity(text.len());
        let mut rebuilt_elements = Vec::with_capacity(elements.len());
        let mut cursor = 0usize;

        for elem in elements {
            let start = elem.byte_range.start.min(text.len());
            let end = elem.byte_range.end.min(text.len());
            if start > end {
                continue;
            }
            if start > cursor {
                rebuilt.push_str(&text[cursor..start]);
            }
            let elem_text = &text[start..end];
            let placeholder = elem.placeholder(text).map(str::to_string);
            let replacement = placeholder
                .as_deref()
                .and_then(|ph| pending_by_placeholder.get_mut(ph))
                .and_then(VecDeque::pop_front);
            if let Some(actual) = replacement {
                // Stage 3: inline actual paste payloads and drop their placeholder elements.
                rebuilt.push_str(actual);
            } else {
                // Stage 4: keep non-paste elements, updating their byte ranges for the new text.
                let new_start = rebuilt.len();
                rebuilt.push_str(elem_text);
                let new_end = rebuilt.len();
                let placeholder = placeholder.or_else(|| Some(elem_text.to_string()));
                rebuilt_elements.push(TextElement::new(
                    ByteRange {
                        start: new_start,
                        end: new_end,
                    },
                    placeholder,
                ));
            }
            cursor = end;
        }

        // Stage 5: append any trailing text that followed the last element.
        if cursor < text.len() {
            rebuilt.push_str(&text[cursor..]);
        }

        (rebuilt, rebuilt_elements)
    }
}
