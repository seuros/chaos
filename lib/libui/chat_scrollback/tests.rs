use super::*;
use crate::history_cell::PlainHistoryCell;
use pretty_assertions::assert_eq;
use ratatui::style::{Color, Stylize};

#[test]
fn mermaid_keeps_source_and_only_uses_retained_graphics_with_capabilities() {
    use crate::history_cell::AgentMessageCell;
    use crate::markdown_render::MarkdownPart;
    use crate::tui::FrameRequester;
    use ratatui_image::picker::{Picker, ProtocolType};

    let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(AgentMessageCell::from_parts(
        vec![MarkdownPart::Mermaid {
            source: "source".into(),
            fallback: vec![Line::from("literal source")],
        }],
        true,
    ))];
    let mut view = ChatScrollback::default();
    view.prepare_graphics(&cells, FrameRequester::test_dummy(), None);
    view.sync(&cells, 40, None, |_| Vec::new());
    assert!(!view.uses_retained_view());
    assert!(first_row(&mut view, 40, 4).contains("literal source"));

    let mut picker = Picker::halfblocks();
    picker.set_protocol_type(ProtocolType::Iterm2);
    view.prepare_graphics(&cells, FrameRequester::test_dummy(), Some(&picker));
    view.sync(&cells, 40, None, |_| Vec::new());
    view.follow_live();
    assert!(view.uses_retained_view());
    assert!(!view.is_scrolled());
    // Without a worker result, the source must still be readable.
    assert!(first_row(&mut view, 40, 4).contains("literal source"));
    assert!(
        cells[0].transcript_lines(40)[0]
            .to_string()
            .contains("literal source")
    );

    view.prepare_graphics(&[], FrameRequester::test_dummy(), Some(&picker));
    assert!(!view.uses_retained_view());
    assert!(view.mermaid.is_none());
}

fn history(rows: usize) -> Vec<Arc<dyn HistoryCell>> {
    vec![Arc::new(PlainHistoryCell::new(
        (0..rows)
            .map(|row| Line::from(format!("row {row}")))
            .collect(),
    ))]
}

fn first_row(view: &mut ChatScrollback, width: u16, height: u16) -> String {
    let area = Rect::new(3, 2, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, true);
    (area.x..area.right())
        .map(|x| buf[(x, area.y)].symbol())
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[test]
fn scrollback_navigation_clamps_and_resumes_live_output() {
    let mut view = ChatScrollback::default();
    view.sync(&history(60), 20, None, |_| Vec::new());
    assert!(!view.is_scrolled());
    view.scroll(Scroll::Down(3), 10);
    assert!(!view.is_scrolled());
    view.scroll(Scroll::Up(3), 10);
    assert_eq!(first_row(&mut view, 20, 10), "row 47");
    view.scroll(Scroll::Up(usize::MAX), 10);
    assert_eq!(first_row(&mut view, 20, 10), "row 0");
    view.scroll(Scroll::Down(usize::MAX), 10);
    assert!(!view.is_scrolled());
    assert_eq!(first_row(&mut view, 20, 10), "row 50");
    view.scroll(Scroll::Start, 10);
    assert!(view.is_scrolled());
    view.scroll(Scroll::End, 10);
    assert!(!view.is_scrolled());

    view.sync(&history(2), 20, None, |_| Vec::new());
    view.scroll(Scroll::Up(3), 10);
    assert!(!view.is_scrolled());
    view.sync(&[], 0, None, |_| Vec::new());
    view.scroll(Scroll::Start, 0);
    assert!(!view.is_scrolled());
    view.render(Rect::default(), &mut Buffer::empty(Rect::default()), true);
}

#[test]
fn scrollback_preserves_reading_position_when_history_and_live_tail_grow() {
    let mut view = ChatScrollback::default();
    let mut cells = history(60);
    let key = ActiveCellTranscriptKey {
        revision: 1,
        is_stream_continuation: false,
        animation_tick: None,
    };
    view.sync(&cells, 20, Some(key), |_| vec!["live".into()]);
    view.scroll(Scroll::Up(10), 10);
    let before = first_row(&mut view, 20, 10);
    cells.push(Arc::new(PlainHistoryCell::new(vec!["committed".into()])));
    view.sync(
        &cells,
        20,
        Some(ActiveCellTranscriptKey { revision: 2, ..key }),
        |_| vec!["live".into(), "more output".into()],
    );
    assert_eq!(first_row(&mut view, 20, 10), before);
    view.sync(
        &cells,
        20,
        Some(ActiveCellTranscriptKey { revision: 2, ..key }),
        |_| panic!("unchanged live tail should remain cached"),
    );
    view.scroll(Scroll::End, 10);
    assert_eq!(view.live.last().unwrap().to_string(), "more output");
    assert!(!view.is_scrolled());

    // Trimming history and dropping the active cell clamps stale positions.
    view.scroll(Scroll::Up(3), 10);
    view.sync(&history(2), 20, None, |_| Vec::new());
    assert_eq!(first_row(&mut view, 20, 10), "row 0");
    assert!(view.live.is_empty());
}

#[test]
fn scrollback_counts_wrapped_rows_and_reflows_without_losing_styles() {
    let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(PlainHistoryCell::new(vec![
        Line::from("abcdefghij".red()),
        Line::from("界界界界界"),
    ]))];
    let mut view = ChatScrollback::default();
    view.sync(&cells, 5, None, |_| Vec::new());
    assert!(view.history.len() >= 5);
    assert!(view.history.iter().all(|line| line.width() <= 5));
    view.scroll(Scroll::Start, 2);
    assert_eq!(first_row(&mut view, 5, 2), "abcde");
    assert_eq!(view.history[0].spans[0].style.fg, Some(Color::Red));
    view.sync(&cells, 10, None, |_| Vec::new());
    assert_eq!(view.history.len(), 2);
    assert_eq!(first_row(&mut view, 10, 2), "abcdefghij");
}

#[test]
fn scrollback_includes_live_only_output_and_histories_beyond_u16_rows() {
    let mut view = ChatScrollback::default();
    let key = ActiveCellTranscriptKey {
        revision: 1,
        is_stream_continuation: false,
        animation_tick: None,
    };
    view.sync(&[], 20, Some(key), |_| {
        (0..40)
            .map(|row| Line::from(format!("live {row}")))
            .collect()
    });
    view.scroll(Scroll::Up(3), 10);
    assert_eq!(first_row(&mut view, 20, 10), "live 27");
    view.sync(&history(70_000), 20, None, |_| Vec::new());
    assert!(view.live.is_empty());
    view.follow_live();
    view.scroll(Scroll::Up(3), 10);
    assert_eq!(first_row(&mut view, 20, 10), "row 69987");
}

#[test]
fn scrollback_uses_compact_cells_and_keeps_stream_continuations_together() {
    #[derive(Debug)]
    struct CompactCell(&'static str, bool);
    impl HistoryCell for CompactCell {
        fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
            vec![self.0.into()]
        }

        fn transcript_lines(&self, _width: u16) -> Vec<Line<'static>> {
            panic!("in-pane scrolling must not expand transcript-only output")
        }

        fn is_stream_continuation(&self) -> bool {
            self.1
        }
    }
    let cells: Vec<Arc<dyn HistoryCell>> = vec![
        Arc::new(CompactCell("first", false)),
        Arc::new(CompactCell("continued", true)),
        Arc::new(CompactCell("next", false)),
    ];
    let mut view = ChatScrollback::default();
    view.sync(&cells, 20, None, |_| Vec::new());
    assert_eq!(
        view.history.iter().map(Line::to_string).collect::<Vec<_>>(),
        ["first", "continued", "", "next"],
    );
}
