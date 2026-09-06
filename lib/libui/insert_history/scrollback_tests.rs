use super::*;
use crate::custom_terminal::Terminal;
use crate::test_backend::VT100Backend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};

const WIDTH: u16 = 24;
const HEIGHT: u16 = 8;

fn draw_chrome(terminal: &mut Terminal<VT100Backend>, reserved: u16) {
    for row in 0..reserved {
        let area = Rect::new(0, row, WIDTH, 1);
        let mut buffer = Buffer::empty(area);
        Line::from(format!("header {row}")).render(area, &mut buffer);
        terminal.draw_pinned_row(&buffer).unwrap();
    }
    terminal
        .draw(|frame| {
            let lines: Vec<Line> = (0..frame.area().height)
                .map(|row| Line::from(format!("draft {row}")))
                .collect();
            frame.render_widget(Paragraph::new(lines), frame.area());
        })
        .unwrap();
}

fn assert_thread(terminal: &Terminal<VT100Backend>, reserved: u16, expected: &[String]) {
    let mut screen = terminal.backend().vt100().screen().clone();
    screen.set_scrollback(usize::MAX);
    let scrollback_len = screen.scrollback();
    let visible_history = terminal.viewport_area.top() - reserved;
    assert_eq!(
        scrollback_len + usize::from(visible_history),
        expected.len()
    );

    let mut history = Vec::new();
    for offset in (1..=scrollback_len).rev() {
        screen.set_scrollback(offset);
        history.push(screen.rows(0, WIDTH).next().unwrap());
    }
    screen.set_scrollback(0);
    let rows: Vec<String> = screen.rows(0, WIDTH).collect();
    history.extend(
        rows[usize::from(reserved)..usize::from(terminal.viewport_area.top())]
            .iter()
            .cloned(),
    );
    assert_eq!(history, expected);
    for row in 0..reserved {
        assert_eq!(rows[usize::from(row)], format!("header {row}"));
    }
    for row in 0..terminal.viewport_area.height {
        assert_eq!(
            rows[usize::from(terminal.viewport_area.top() + row)],
            format!("draft {row}")
        );
    }
}

#[test]
fn native_scrollback_retains_thread_and_excludes_chrome() {
    for reserved in [0, 1, 2] {
        for viewport_height in [1, 3, HEIGHT - reserved] {
            for batch_size in [1, 4, 30] {
                let backend = VT100Backend::with_scrollback(WIDTH, HEIGHT, 100);
                let mut terminal = Terminal::with_options(backend).unwrap();
                terminal.set_viewport_area(Rect::new(0, reserved, WIDTH, viewport_height));
                draw_chrome(&mut terminal, reserved);

                let expected: Vec<String> = (0..30).map(|row| format!("message {row}")).collect();
                let mut inserted = 0;
                for batch in expected.chunks(batch_size) {
                    insert_history_lines_with_reserved(
                        &mut terminal,
                        batch.iter().cloned().map(Line::from).collect(),
                        reserved,
                    )
                    .unwrap();
                    inserted += batch.len();
                    draw_chrome(&mut terminal, reserved);
                    assert_thread(&terminal, reserved, &expected[..inserted]);
                }
            }
        }
    }
}

#[test]
fn native_scrollback_survives_composer_growth() {
    for reserved in [0, 1, 2] {
        let backend = VT100Backend::with_scrollback(WIDTH, HEIGHT, 100);
        let mut terminal = Terminal::with_options(backend).unwrap();
        terminal.set_viewport_area(Rect::new(0, reserved, WIDTH, 1));
        draw_chrome(&mut terminal, reserved);
        let mut expected: Vec<String> = (0..14).map(|row| format!("message {row}")).collect();
        insert_history_lines_with_reserved(
            &mut terminal,
            expected.iter().cloned().map(Line::from).collect(),
            reserved,
        )
        .unwrap();
        draw_chrome(&mut terminal, reserved);

        for new_height in 2..=HEIGHT - reserved {
            let mut area = terminal.viewport_area;
            area.height = new_height;
            scroll_history_up(&mut terminal, area.bottom() - HEIGHT, reserved).unwrap();
            area.y = HEIGHT - new_height;
            terminal.set_viewport_area(area);
            draw_chrome(&mut terminal, reserved);
            assert_thread(&terminal, reserved, &expected);
        }

        expected.push("after growth".to_string());
        insert_history_lines_with_reserved(
            &mut terminal,
            vec![Line::from("after growth")],
            reserved,
        )
        .unwrap();
        draw_chrome(&mut terminal, reserved);
        assert_thread(&terminal, reserved, &expected);
    }
}

#[test]
fn native_scrollback_preserves_wrapped_urls_and_blank_rows() {
    for reserved in [0, 1] {
        let backend = VT100Backend::with_scrollback(WIDTH, HEIGHT, 100);
        let mut terminal = Terminal::with_options(backend).unwrap();
        terminal.set_viewport_area(Rect::new(0, reserved, WIDTH, 2));
        draw_chrome(&mut terminal, reserved);
        let prefix = "https://example.com/";
        let url = format!(
            "{prefix}{}",
            "x".repeat(usize::from(WIDTH) * 10 - prefix.len())
        );
        let mut expected: Vec<String> = url
            .as_bytes()
            .chunks(usize::from(WIDTH))
            .map(|row| String::from_utf8(row.to_vec()).unwrap())
            .collect();
        expected.extend([String::new(), "after URL".to_string()]);

        insert_history_lines_with_reserved(
            &mut terminal,
            vec![Line::from(url), Line::from(""), Line::from("after URL")],
            reserved,
        )
        .unwrap();
        draw_chrome(&mut terminal, reserved);

        assert_thread(&terminal, reserved, &expected);
    }
}

#[test]
fn empty_history_keeps_screen_and_cursor_unchanged() {
    let backend = VT100Backend::with_scrollback(WIDTH, HEIGHT, 100);
    let mut terminal = Terminal::with_options(backend).unwrap();
    terminal.set_viewport_area(Rect::new(0, 1, WIDTH, 3));
    draw_chrome(&mut terminal, 1);
    let before = terminal.backend().vt100().screen().state_formatted();
    let viewport = terminal.viewport_area;

    insert_history_lines_with_reserved(&mut terminal, vec![], 1).unwrap();

    assert_eq!(
        terminal.backend().vt100().screen().state_formatted(),
        before
    );
    assert_eq!(terminal.viewport_area, viewport);
}
