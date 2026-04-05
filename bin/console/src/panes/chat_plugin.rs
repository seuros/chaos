use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui_hypertile_extras::HypertilePlugin;

pub(crate) struct ChatPlugin;

impl HypertilePlugin for ChatPlugin {
    fn render(&self, _area: Rect, _buf: &mut Buffer, _is_focused: bool) {
        // Chat content is rendered externally by App after TileManager::render().
    }
}
