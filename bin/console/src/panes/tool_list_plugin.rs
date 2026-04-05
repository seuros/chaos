use crate::panes::tool_list::ToolListKeyResult;
use crate::panes::tool_list::ToolListPane;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui_hypertile::EventOutcome;
use ratatui_hypertile::HypertileEvent;
use ratatui_hypertile_extras::HypertilePlugin;
use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;

pub(crate) struct ToolListPlugin {
    state: Rc<RefCell<ToolListPane>>,
    close_requested: Rc<Cell<bool>>,
}

impl ToolListPlugin {
    pub(crate) fn new(state: Rc<RefCell<ToolListPane>>, close_requested: Rc<Cell<bool>>) -> Self {
        Self {
            state,
            close_requested,
        }
    }
}

impl HypertilePlugin for ToolListPlugin {
    fn render(&self, area: Rect, buf: &mut Buffer, is_focused: bool) {
        self.state.borrow().render(area, buf, is_focused);
    }

    fn on_event(&mut self, event: &HypertileEvent) -> EventOutcome {
        match event {
            HypertileEvent::Key(chord) => match self.state.borrow_mut().handle_chord(*chord) {
                ToolListKeyResult::Consumed => EventOutcome::Consumed,
                ToolListKeyResult::Close => {
                    self.close_requested.set(true);
                    EventOutcome::Consumed
                }
                ToolListKeyResult::Ignored => EventOutcome::Ignored,
            },
            _ => EventOutcome::Ignored,
        }
    }
}
