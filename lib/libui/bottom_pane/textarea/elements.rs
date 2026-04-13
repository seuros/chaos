use std::ops::Range;

use chaos_ipc::user_input::ByteRange;
use chaos_ipc::user_input::TextElement as UserTextElement;

use super::core::{TextArea, TextElement, TextElementSnapshot};

impl TextArea {
    // ===== Text elements support =====

    pub fn element_payloads(&self) -> Vec<String> {
        self.elements
            .iter()
            .filter_map(|e| self.text.get(e.range.clone()).map(str::to_string))
            .collect()
    }

    pub fn text_elements(&self) -> Vec<UserTextElement> {
        self.elements
            .iter()
            .map(|e| {
                let placeholder = self.text.get(e.range.clone()).map(str::to_string);
                UserTextElement::new(
                    ByteRange {
                        start: e.range.start,
                        end: e.range.end,
                    },
                    placeholder,
                )
            })
            .collect()
    }

    pub fn text_element_snapshots(&self) -> Vec<TextElementSnapshot> {
        self.elements
            .iter()
            .filter_map(|element| {
                self.text
                    .get(element.range.clone())
                    .map(|text| TextElementSnapshot {
                        id: element.id,
                        range: element.range.clone(),
                        text: text.to_string(),
                    })
            })
            .collect()
    }

    pub fn element_id_for_exact_range(&self, range: Range<usize>) -> Option<u64> {
        self.elements
            .iter()
            .find(|element| element.range == range)
            .map(|element| element.id)
    }

    /// Renames a single text element in-place, keeping it atomic.
    ///
    /// Use this when the element payload is an identifier (e.g. a placeholder) that must be
    /// updated without converting the element back into normal text.
    pub fn replace_element_payload(&mut self, old: &str, new: &str) -> bool {
        let Some(idx) = self
            .elements
            .iter()
            .position(|e| self.text.get(e.range.clone()) == Some(old))
        else {
            return false;
        };

        let range = self.elements[idx].range.clone();
        let start = range.start;
        let end = range.end;
        if start > end || end > self.text.len() {
            return false;
        }

        let removed_len = end - start;
        let inserted_len = new.len();
        let diff = inserted_len as isize - removed_len as isize;

        self.text.replace_range(range, new);
        self.wrap_cache.replace(None);
        self.preferred_col = None;

        // Update the modified element's range.
        self.elements[idx].range = start..(start + inserted_len);

        // Shift element ranges that occur after the replaced element.
        if diff != 0 {
            for (j, e) in self.elements.iter_mut().enumerate() {
                if j == idx {
                    continue;
                }
                if e.range.end <= start {
                    continue;
                }
                if e.range.start >= end {
                    e.range.start = ((e.range.start as isize) + diff) as usize;
                    e.range.end = ((e.range.end as isize) + diff) as usize;
                    continue;
                }

                // Elements should not partially overlap each other; degrade gracefully by
                // snapping anything intersecting the replaced range to the new bounds.
                e.range.start = start.min(e.range.start);
                e.range.end = (start + inserted_len).max(e.range.end.saturating_add_signed(diff));
            }
        }

        // Update the cursor position to account for the edit.
        self.cursor_pos = if self.cursor_pos < start {
            self.cursor_pos
        } else if self.cursor_pos <= end {
            start + inserted_len
        } else {
            ((self.cursor_pos as isize) + diff) as usize
        };
        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);

        // Keep element ordering deterministic.
        self.elements.sort_by_key(|e| e.range.start);

        true
    }

    pub fn insert_element(&mut self, text: &str) -> u64 {
        let start = self.clamp_pos_for_insertion(self.cursor_pos);
        self.insert_str_at(start, text);
        let end = start + text.len();
        let id = self.add_element(start..end);
        // Place cursor at end of inserted element
        self.set_cursor(end);
        id
    }

    pub(super) fn add_element(&mut self, range: Range<usize>) -> u64 {
        let id = self.next_element_id();
        self.elements.push(TextElement { id, range });
        self.elements.sort_by_key(|e| e.range.start);
        id
    }

    /// Mark an existing text range as an atomic element without changing the text.
    ///
    /// This is used to convert already-typed tokens (like `/plan`) into elements
    /// so they render and edit atomically. Overlapping or duplicate ranges are ignored.
    pub fn add_element_range(&mut self, range: Range<usize>) -> Option<u64> {
        let start = self.clamp_pos_to_char_boundary(range.start.min(self.text.len()));
        let end = self.clamp_pos_to_char_boundary(range.end.min(self.text.len()));
        if start >= end {
            return None;
        }
        if self
            .elements
            .iter()
            .any(|e| e.range.start == start && e.range.end == end)
        {
            return None;
        }
        if self
            .elements
            .iter()
            .any(|e| start < e.range.end && end > e.range.start)
        {
            return None;
        }
        let id = self.add_element(start..end);
        Some(id)
    }

    pub fn remove_element_range(&mut self, range: Range<usize>) -> bool {
        let start = self.clamp_pos_to_char_boundary(range.start.min(self.text.len()));
        let end = self.clamp_pos_to_char_boundary(range.end.min(self.text.len()));
        if start >= end {
            return false;
        }
        let len_before = self.elements.len();
        self.elements
            .retain(|elem| elem.range.start != start || elem.range.end != end);
        len_before != self.elements.len()
    }

    pub(super) fn expand_range_to_element_boundaries(
        &self,
        mut range: Range<usize>,
    ) -> Range<usize> {
        // Expand to include any intersecting elements fully
        loop {
            let mut changed = false;
            for e in &self.elements {
                if e.range.start < range.end && e.range.end > range.start {
                    let new_start = range.start.min(e.range.start);
                    let new_end = range.end.max(e.range.end);
                    if new_start != range.start || new_end != range.end {
                        range.start = new_start;
                        range.end = new_end;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        range
    }

    pub(super) fn shift_elements(&mut self, at: usize, removed: usize, inserted: usize) {
        // Generic shift: for pure insert, removed = 0; for delete, inserted = 0.
        let end = at + removed;
        let diff = inserted as isize - removed as isize;
        // Remove elements fully deleted by the operation and shift the rest
        self.elements
            .retain(|e| !(e.range.start >= at && e.range.end <= end));
        for e in &mut self.elements {
            if e.range.end <= at {
                // before edit
            } else if e.range.start >= end {
                // after edit
                e.range.start = ((e.range.start as isize) + diff) as usize;
                e.range.end = ((e.range.end as isize) + diff) as usize;
            } else {
                // Overlap with element but not fully contained (shouldn't happen when using
                // element-aware replace, but degrade gracefully by snapping element to new bounds)
                let new_start = at.min(e.range.start);
                let new_end = at + inserted.max(e.range.end.saturating_sub(end));
                e.range.start = new_start;
                e.range.end = new_end;
            }
        }
    }

    pub(super) fn update_elements_after_replace(
        &mut self,
        start: usize,
        end: usize,
        inserted_len: usize,
    ) {
        self.shift_elements(start, end.saturating_sub(start), inserted_len);
    }
}
