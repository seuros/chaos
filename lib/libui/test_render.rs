use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::render::renderable::Renderable;

pub fn buffer_to_first_char_string(buf: &Buffer) -> String {
    let area = buf.area();
    let mut lines = Vec::new();
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            row.push(
                buf[(area.x + x, area.y + y)]
                    .symbol()
                    .chars()
                    .next()
                    .unwrap_or(' '),
            );
        }
        lines.push(row);
    }
    lines.join("\n")
}

pub fn buffer_to_string(buf: &Buffer) -> String {
    let area = buf.area();
    (0..area.height)
        .map(|row| {
            let mut line = String::new();
            for col in 0..area.width {
                let symbol = buf[(area.x + col, area.y + row)].symbol();
                if symbol.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(symbol);
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn buffer_to_trimmed_string(buf: &Buffer) -> String {
    let mut lines: Vec<String> = buffer_to_string(buf)
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect();

    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }

    lines.join("\n")
}

pub fn render_to_first_char_string(renderable: &impl Renderable, area: Rect) -> String {
    let mut buf = Buffer::empty(area);
    renderable.render(area, &mut buf);
    buffer_to_first_char_string(&buf)
}

pub fn render_to_string(renderable: &impl Renderable, area: Rect) -> String {
    let mut buf = Buffer::empty(area);
    renderable.render(area, &mut buf);
    buffer_to_string(&buf)
}

pub fn render_to_trimmed_string(renderable: &impl Renderable, area: Rect) -> String {
    let mut buf = Buffer::empty(area);
    renderable.render(area, &mut buf);
    buffer_to_trimmed_string(&buf)
}
