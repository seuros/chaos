/// Generic scroll/selection state for a vertical list menu.
///
/// Encapsulates the common behavior of a selectable list that supports:
/// - Optional selection (None when list is empty)
/// - Wrap-around navigation on Up/Down
/// - Maintaining a scroll window (`scroll_top`) so the selected row stays visible
#[derive(Debug, Default, Clone, Copy)]
pub struct ScrollState {
    pub selected_idx: Option<usize>,
    pub scroll_top: usize,
}

impl ScrollState {
    pub fn new() -> Self {
        Self {
            selected_idx: None,
            scroll_top: 0,
        }
    }

    /// Reset selection and scroll.
    pub fn reset(&mut self) {
        self.selected_idx = None;
        self.scroll_top = 0;
    }

    /// Clamp selection to be within the [0, len-1] range, or None when empty.
    pub fn clamp_selection(&mut self, len: usize) {
        self.selected_idx = match len {
            0 => None,
            _ => Some(self.selected_idx.unwrap_or(0).min(len - 1)),
        };
        if len == 0 {
            self.scroll_top = 0;
        }
    }

    /// Move selection up by one, wrapping to the bottom when necessary.
    pub fn move_up_wrap(&mut self, len: usize) {
        if len == 0 {
            self.selected_idx = None;
            self.scroll_top = 0;
            return;
        }
        self.selected_idx = Some(match self.selected_idx {
            Some(idx) if idx > 0 => idx - 1,
            Some(_) => len - 1,
            None => 0,
        });
    }

    /// Move selection down by one, wrapping to the top when necessary.
    pub fn move_down_wrap(&mut self, len: usize) {
        if len == 0 {
            self.selected_idx = None;
            self.scroll_top = 0;
            return;
        }
        self.selected_idx = Some(match self.selected_idx {
            Some(idx) if idx + 1 < len => idx + 1,
            _ => 0,
        });
    }

    /// Move up one step and rescroll the viewport in a single call.
    ///
    /// Equivalent to `move_up_wrap(len)` followed by
    /// `ensure_visible(len, visible_rows)`. The two calls are always paired at
    /// call sites that display a scrolling list, so the fused helper lets
    /// popups write the intent — step the cursor — without repeating the
    /// scroll bookkeeping that always has to follow.
    pub fn step_up(&mut self, len: usize, visible_rows: usize) {
        self.move_up_wrap(len);
        self.ensure_visible(len, visible_rows);
    }

    /// Move down one step and rescroll the viewport in a single call.
    /// See `step_up` for the rationale.
    pub fn step_down(&mut self, len: usize, visible_rows: usize) {
        self.move_down_wrap(len);
        self.ensure_visible(len, visible_rows);
    }

    /// Clamp the selection to the current list length and rescroll in a
    /// single call. Used after the item list changes (a new filter, a fresh
    /// set of matches) to keep the cursor in range and visible.
    pub fn sync_visible(&mut self, len: usize, visible_rows: usize) {
        self.clamp_selection(len);
        self.ensure_visible(len, visible_rows);
    }

    /// Adjust `scroll_top` so that the current `selected_idx` is visible within
    /// the window of `visible_rows`.
    pub fn ensure_visible(&mut self, len: usize, visible_rows: usize) {
        if len == 0 || visible_rows == 0 {
            self.scroll_top = 0;
            return;
        }
        if let Some(sel) = self.selected_idx {
            if sel < self.scroll_top {
                self.scroll_top = sel;
            } else {
                let bottom = self.scroll_top + visible_rows - 1;
                if sel > bottom {
                    self.scroll_top = sel + 1 - visible_rows;
                }
            }
        } else {
            self.scroll_top = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ScrollState;

    #[test]
    fn wrap_navigation_and_visibility() {
        let mut s = ScrollState::new();
        let len = 10;
        let vis = 5;

        s.clamp_selection(len);
        assert_eq!(s.selected_idx, Some(0));
        s.ensure_visible(len, vis);
        assert_eq!(s.scroll_top, 0);

        s.move_up_wrap(len);
        s.ensure_visible(len, vis);
        assert_eq!(s.selected_idx, Some(len - 1));
        match s.selected_idx {
            Some(sel) => assert!(s.scroll_top <= sel),
            None => panic!("expected Some(selected_idx) after wrap"),
        }

        s.move_down_wrap(len);
        s.ensure_visible(len, vis);
        assert_eq!(s.selected_idx, Some(0));
        assert_eq!(s.scroll_top, 0);
    }
}
