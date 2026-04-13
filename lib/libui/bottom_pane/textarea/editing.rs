use std::ops::Range;

use super::core::TextArea;

impl TextArea {
    pub fn insert_str(&mut self, text: &str) {
        self.insert_str_at(self.cursor_pos, text);
    }

    pub fn insert_str_at(&mut self, pos: usize, text: &str) {
        let pos = self.clamp_pos_for_insertion(pos);
        self.text.insert_str(pos, text);
        self.wrap_cache.replace(None);
        if pos <= self.cursor_pos {
            self.cursor_pos += text.len();
        }
        self.shift_elements(pos, /*removed*/ 0, text.len());
        self.preferred_col = None;
    }

    pub fn replace_range(&mut self, range: Range<usize>, text: &str) {
        let range = self.expand_range_to_element_boundaries(range);
        self.replace_range_raw(range, text);
    }

    pub(super) fn replace_range_raw(&mut self, range: Range<usize>, text: &str) {
        assert!(range.start <= range.end);
        let start = range.start.clamp(0, self.text.len());
        let end = range.end.clamp(0, self.text.len());
        let removed_len = end - start;
        let inserted_len = text.len();
        if removed_len == 0 && inserted_len == 0 {
            return;
        }
        let diff = inserted_len as isize - removed_len as isize;

        self.text.replace_range(range, text);
        self.wrap_cache.replace(None);
        self.preferred_col = None;
        self.update_elements_after_replace(start, end, inserted_len);

        // Update the cursor position to account for the edit.
        self.cursor_pos = if self.cursor_pos < start {
            // Cursor was before the edited range – no shift.
            self.cursor_pos
        } else if self.cursor_pos <= end {
            // Cursor was inside the replaced range – move to end of the new text.
            start + inserted_len
        } else {
            // Cursor was after the replaced range – shift by the length diff.
            ((self.cursor_pos as isize) + diff) as usize
        }
        .min(self.text.len());

        // Ensure cursor is not inside an element
        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
    }

    pub fn delete_backward(&mut self, n: usize) {
        if n == 0 || self.cursor_pos == 0 {
            return;
        }
        let mut target = self.cursor_pos;
        for _ in 0..n {
            target = self.prev_atomic_boundary(target);
            if target == 0 {
                break;
            }
        }
        self.replace_range(target..self.cursor_pos, "");
    }

    pub fn delete_forward(&mut self, n: usize) {
        if n == 0 || self.cursor_pos >= self.text.len() {
            return;
        }
        let mut target = self.cursor_pos;
        for _ in 0..n {
            target = self.next_atomic_boundary(target);
            if target >= self.text.len() {
                break;
            }
        }
        self.replace_range(self.cursor_pos..target, "");
    }

    pub fn delete_backward_word(&mut self) {
        let start = self.beginning_of_previous_word();
        self.kill_range(start..self.cursor_pos);
    }

    /// Delete text to the right of the cursor using "word" semantics.
    ///
    /// Deletes from the current cursor position through the end of the next word as determined
    /// by `end_of_next_word()`. Any whitespace (including newlines) between the cursor and that
    /// word is included in the deletion.
    pub fn delete_forward_word(&mut self) {
        let end = self.end_of_next_word();
        if end > self.cursor_pos {
            self.kill_range(self.cursor_pos..end);
        }
    }

    /// Kill from the cursor to the end of the current logical line.
    ///
    /// If the cursor is already at end-of-line and a trailing newline exists, this kills that
    /// newline so repeated invocations continue making progress. The removed text becomes the next
    /// yank target and remains available even if a caller later clears or rewrites the visible
    /// buffer via `set_text_*`.
    pub fn kill_to_end_of_line(&mut self) {
        let eol = self.end_of_current_line();
        let range = if self.cursor_pos == eol {
            if eol < self.text.len() {
                Some(self.cursor_pos..eol + 1)
            } else {
                None
            }
        } else {
            Some(self.cursor_pos..eol)
        };

        if let Some(range) = range {
            self.kill_range(range);
        }
    }

    pub fn kill_to_beginning_of_line(&mut self) {
        let bol = self.beginning_of_current_line();
        let range = if self.cursor_pos == bol {
            if bol > 0 { Some(bol - 1..bol) } else { None }
        } else {
            Some(bol..self.cursor_pos)
        };

        if let Some(range) = range {
            self.kill_range(range);
        }
    }

    /// Insert the most recently killed text at the cursor.
    ///
    /// This uses the textarea's single-entry kill buffer. Because whole-buffer replacement APIs do
    /// not clear that buffer, `yank` can restore text after composer-level clears such as submit
    /// and slash-command dispatch.
    pub fn yank(&mut self) {
        if self.kill_buffer.is_empty() {
            return;
        }
        let text = self.kill_buffer.clone();
        self.insert_str(&text);
    }

    pub(super) fn kill_range(&mut self, range: Range<usize>) {
        let range = self.expand_range_to_element_boundaries(range);
        if range.start >= range.end {
            return;
        }

        let removed = self.text[range.clone()].to_string();
        if removed.is_empty() {
            return;
        }

        self.kill_buffer = removed;
        self.replace_range_raw(range, "");
    }
}
