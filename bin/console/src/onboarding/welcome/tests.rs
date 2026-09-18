use super::*;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn row_containing(buf: &Buffer, needle: &str) -> Option<u16> {
    (0..buf.area.height).find(|&y| {
        let mut row = String::new();
        for x in 0..buf.area.width {
            row.push_str(buf[(x, y)].symbol());
        }
        row.contains(needle)
    })
}

pub(crate) fn welcome_renders_text_at_top() {
    let widget = WelcomeWidget::new(false);
    let area = Rect::new(0, 0, 80, 20);
    let mut buf = Buffer::empty(area);
    (&widget).render(area, &mut buf);

    let welcome_row = row_containing(&buf, "Welcome");
    assert_eq!(welcome_row, Some(0));
}
