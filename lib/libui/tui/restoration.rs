use super::{RestoreMode, Tui, flush_terminal_input_buffer, set_modes};
use std::sync::atomic::Ordering;

pub(super) struct RestoredTerminal<'a> {
    tui: &'a mut Tui,
    alt_screen: bool,
    mouse_capture: bool,
}

impl<'a> RestoredTerminal<'a> {
    pub(super) fn new(tui: &'a mut Tui, mode: RestoreMode) -> Self {
        tui.pause_events();
        let guard = Self {
            alt_screen: tui.is_alt_screen_active(),
            mouse_capture: tui.mouse_capture_active.load(Ordering::Relaxed),
            tui,
        };
        if guard.alt_screen {
            let _ = guard.tui.leave_alt_screen();
        } else if guard.mouse_capture {
            guard.tui.disable_mouse_capture();
        }
        if let Err(err) = mode.restore() {
            tracing::warn!("failed to restore terminal modes before external program: {err}");
        }
        guard
    }
}

impl Drop for RestoredTerminal<'_> {
    fn drop(&mut self) {
        if let Err(err) = set_modes() {
            tracing::warn!("failed to re-enable terminal modes after external program: {err}");
        }
        flush_terminal_input_buffer();
        if self.alt_screen {
            let _ = self.tui.enter_alt_screen();
        }
        if self.mouse_capture {
            self.tui.enable_mouse_capture();
        }
        self.tui.resume_events();
    }
}
