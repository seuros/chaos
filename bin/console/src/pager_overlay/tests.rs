use super::*;
use chaos_ipc::protocol::ExecCommandSource;
use chaos_ipc::protocol::ReviewDecision;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::chatwidget::ActiveCellTranscriptKey;
use crate::exec_cell::CommandOutput;
use crate::history_cell;
use crate::history_cell::HistoryCell;
use crate::history_cell::new_patch_event;
use chaos_ipc::parse_command::ParsedCommand;
use chaos_ipc::protocol::FileChange;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::text::Text;

#[derive(Debug)]
struct TestCell {
    lines: Vec<Line<'static>>,
}

impl crate::history_cell::HistoryCell for TestCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.clone()
    }

    fn transcript_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.clone()
    }

    fn desired_transcript_height(&self, width: u16) -> u16 {
        let lines = self.transcript_lines(width);
        lines.len().try_into().unwrap_or(u16::MAX)
    }

    fn is_stream_continuation(&self) -> bool {
        false
    }
}

fn paragraph_block(label: &str, lines: usize) -> Box<dyn Renderable> {
    let text = Text::from(
        (0..lines)
            .map(|i| Line::from(format!("{label}{i}")))
            .collect::<Vec<_>>(),
    );
    Box::new(ratatui::widgets::Paragraph::new(text)) as Box<dyn Renderable>
}

pub(crate) fn pager_overlay_suite() {
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("../snapshots");
    let _snapshot_settings = settings.bind_to_scope();

    edit_prev_hint_is_visible();
    edit_next_hint_is_visible_when_highlighted();
    transcript_overlay_snapshot_basic();
    transcript_overlay_renders_live_tail();
    transcript_overlay_sync_live_tail_is_noop_for_identical_key();
    transcript_overlay_apply_patch_scroll_vt100_clears_previous_page();
    transcript_overlay_keeps_scroll_pinned_at_bottom();
    transcript_overlay_preserves_manual_scroll_position();
    static_overlay_snapshot_basic();
    transcript_overlay_paging_is_continuous_and_round_trips();
    static_overlay_wraps_long_lines();
    pager_view_content_height_counts_renderables();
    pager_view_ensure_chunk_visible_scrolls_down_when_needed();
    pager_view_ensure_chunk_visible_scrolls_up_when_needed();
    pager_view_is_scrolled_to_bottom_accounts_for_wrapped_height();
    pager_view_resolves_bottom_sentinel_to_max_scroll();
    pager_view_mouse_scroll_moves_relative_to_current_position();
    pager_view_mouse_scroll_resolves_bottom_sentinel_before_scrolling_up();
}

fn edit_prev_hint_is_visible() {
    let mut overlay = TranscriptOverlay::new(vec![Arc::new(TestCell {
        lines: vec![Line::from("hello")],
    })]);

    let area = Rect::new(0, 0, 120, 10);
    let mut buf = Buffer::empty(area);
    overlay.render(area, &mut buf);

    let s = buffer_to_text(&buf, area);
    assert!(
        s.contains("edit prev"),
        "expected 'edit prev' hint in overlay footer, got: {s:?}"
    );
}

fn edit_next_hint_is_visible_when_highlighted() {
    let mut overlay = TranscriptOverlay::new(vec![Arc::new(TestCell {
        lines: vec![Line::from("hello")],
    })]);
    overlay.set_highlight_cell(Some(0));

    let area = Rect::new(0, 0, 120, 10);
    let mut buf = Buffer::empty(area);
    overlay.render(area, &mut buf);

    let s = buffer_to_text(&buf, area);
    assert!(
        s.contains("edit next"),
        "expected 'edit next' hint in overlay footer, got: {s:?}"
    );
}

fn transcript_overlay_snapshot_basic() {
    let mut overlay = TranscriptOverlay::new(vec![
        Arc::new(TestCell {
            lines: vec![Line::from("alpha")],
        }),
        Arc::new(TestCell {
            lines: vec![Line::from("beta")],
        }),
        Arc::new(TestCell {
            lines: vec![Line::from("gamma")],
        }),
    ]);
    let mut term = Terminal::new(TestBackend::new(40, 10)).expect("term");
    term.draw(|f| overlay.render(f.area(), f.buffer_mut()))
        .expect("draw");
    assert_snapshot!(term.backend());
}

fn transcript_overlay_renders_live_tail() {
    let mut overlay = TranscriptOverlay::new(vec![Arc::new(TestCell {
        lines: vec![Line::from("alpha")],
    })]);
    overlay.sync_live_tail(
        40,
        Some(ActiveCellTranscriptKey {
            revision: 1,
            is_stream_continuation: false,
            animation_tick: None,
        }),
        |_| Some(vec![Line::from("tail")]),
    );

    let mut term = Terminal::new(TestBackend::new(40, 10)).expect("term");
    term.draw(|f| overlay.render(f.area(), f.buffer_mut()))
        .expect("draw");
    assert_snapshot!(term.backend());
}

fn transcript_overlay_sync_live_tail_is_noop_for_identical_key() {
    let mut overlay = TranscriptOverlay::new(vec![Arc::new(TestCell {
        lines: vec![Line::from("alpha")],
    })]);

    let calls = std::cell::Cell::new(0usize);
    let key = ActiveCellTranscriptKey {
        revision: 1,
        is_stream_continuation: false,
        animation_tick: None,
    };

    overlay.sync_live_tail(40, Some(key), |_| {
        calls.set(calls.get() + 1);
        Some(vec![Line::from("tail")])
    });
    overlay.sync_live_tail(40, Some(key), |_| {
        calls.set(calls.get() + 1);
        Some(vec![Line::from("tail2")])
    });

    assert_eq!(calls.get(), 1);
}

fn buffer_to_text(buf: &Buffer, area: Rect) -> String {
    let mut out = String::new();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let symbol = buf[(x, y)].symbol();
            if symbol.is_empty() {
                out.push(' ');
            } else {
                out.push(symbol.chars().next().unwrap_or(' '));
            }
        }
        while out.ends_with(' ') {
            out.pop();
        }
        out.push('\n');
    }
    out
}

fn transcript_overlay_apply_patch_scroll_vt100_clears_previous_page() {
    let cwd = PathBuf::from("/repo");
    let mut cells: Vec<Arc<dyn HistoryCell>> = Vec::new();

    let mut approval_changes = HashMap::new();
    approval_changes.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\nworld\n".to_string(),
        },
    );
    let approval_cell: Arc<dyn HistoryCell> = Arc::new(new_patch_event(approval_changes, &cwd));
    cells.push(approval_cell);

    let mut apply_changes = HashMap::new();
    apply_changes.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\nworld\n".to_string(),
        },
    );
    let apply_begin_cell: Arc<dyn HistoryCell> = Arc::new(new_patch_event(apply_changes, &cwd));
    cells.push(apply_begin_cell);

    let apply_end_cell: Arc<dyn HistoryCell> = history_cell::new_approval_decision_cell(
        vec!["ls".into()],
        ReviewDecision::Approved,
        history_cell::ApprovalDecisionActor::User,
    )
    .into();
    cells.push(apply_end_cell);

    let mut exec_cell = crate::exec_cell::new_active_exec_command(
        "exec-1".into(),
        vec!["bash".into(), "-lc".into(), "ls".into()],
        vec![ParsedCommand::Unknown { cmd: "ls".into() }],
        ExecCommandSource::Agent,
        None,
        true,
    );
    exec_cell.complete_call(
        "exec-1",
        CommandOutput {
            exit_code: 0,
            aggregated_output: "src\nREADME.md\n".into(),
            formatted_output: "src\nREADME.md\n".into(),
        },
        Duration::from_millis(420),
    );
    let exec_cell: Arc<dyn HistoryCell> = Arc::new(exec_cell);
    cells.push(exec_cell);

    let mut overlay = TranscriptOverlay::new(cells);
    let area = Rect::new(0, 0, 80, 12);
    let mut buf = Buffer::empty(area);

    overlay.render(area, &mut buf);
    overlay.view.scroll_offset = 0;
    overlay.render(area, &mut buf);

    let snapshot = buffer_to_text(&buf, area);
    assert_snapshot!("transcript_overlay_apply_patch_scroll_vt100", snapshot);
}

fn transcript_overlay_keeps_scroll_pinned_at_bottom() {
    let mut overlay = TranscriptOverlay::new(
        (0..20)
            .map(|i| {
                Arc::new(TestCell {
                    lines: vec![Line::from(format!("line{i}"))],
                }) as Arc<dyn HistoryCell>
            })
            .collect(),
    );
    let mut term = Terminal::new(TestBackend::new(40, 12)).expect("term");
    term.draw(|f| overlay.render(f.area(), f.buffer_mut()))
        .expect("draw");

    assert!(
        overlay.view.is_scrolled_to_bottom(),
        "expected initial render to leave view at bottom"
    );

    overlay.insert_cell(Arc::new(TestCell {
        lines: vec!["tail".into()],
    }));

    assert_eq!(overlay.view.scroll_offset, usize::MAX);
}

fn transcript_overlay_preserves_manual_scroll_position() {
    let mut overlay = TranscriptOverlay::new(
        (0..20)
            .map(|i| {
                Arc::new(TestCell {
                    lines: vec![Line::from(format!("line{i}"))],
                }) as Arc<dyn HistoryCell>
            })
            .collect(),
    );
    let mut term = Terminal::new(TestBackend::new(40, 12)).expect("term");
    term.draw(|f| overlay.render(f.area(), f.buffer_mut()))
        .expect("draw");

    overlay.view.scroll_offset = 0;

    overlay.insert_cell(Arc::new(TestCell {
        lines: vec!["tail".into()],
    }));

    assert_eq!(overlay.view.scroll_offset, 0);
}

fn static_overlay_snapshot_basic() {
    let mut overlay = StaticOverlay::with_title(
        vec!["one".into(), "two".into(), "three".into()],
        "S T A T I C".to_string(),
    );
    let mut term = Terminal::new(TestBackend::new(40, 10)).expect("term");
    term.draw(|f| overlay.render(f.area(), f.buffer_mut()))
        .expect("draw");
    assert_snapshot!(term.backend());
}

/// Render transcript overlay and return visible line numbers (`line-NN`) in order.
fn transcript_line_numbers(overlay: &mut TranscriptOverlay, area: Rect) -> Vec<usize> {
    let mut buf = Buffer::empty(area);
    overlay.render(area, &mut buf);

    let top_h = area.height.saturating_sub(3);
    let top = Rect::new(area.x, area.y, area.width, top_h);
    let content_area = overlay.view.content_area(top);

    let mut nums = Vec::new();
    for y in content_area.y..content_area.bottom() {
        let mut line = String::new();
        for x in content_area.x..content_area.right() {
            line.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
        }
        if let Some(n) = line
            .split_whitespace()
            .find_map(|w| w.strip_prefix("line-"))
            .and_then(|s| s.parse().ok())
        {
            nums.push(n);
        }
    }
    nums
}

fn transcript_overlay_paging_is_continuous_and_round_trips() {
    let mut overlay = TranscriptOverlay::new(
        (0..50)
            .map(|i| {
                Arc::new(TestCell {
                    lines: vec![Line::from(format!("line-{i:02}"))],
                }) as Arc<dyn HistoryCell>
            })
            .collect(),
    );
    let area = Rect::new(0, 0, 40, 15);

    let mut buf = Buffer::empty(area);
    overlay.view.scroll_offset = 0;
    overlay.render(area, &mut buf);
    let page_height = overlay.view.page_height(area);

    overlay.view.scroll_offset = 0;
    let page1 = transcript_line_numbers(&mut overlay, area);
    let page1_len = page1.len();
    let expected_page1: Vec<usize> = (0..page1_len).collect();
    assert_eq!(
        page1, expected_page1,
        "first page should start at line-00 and show a full page of content"
    );

    overlay.view.scroll_offset = overlay.view.scroll_offset.saturating_add(page_height);
    let page2 = transcript_line_numbers(&mut overlay, area);
    assert_eq!(
        page2.len(),
        page1_len,
        "second page should have the same number of visible lines as the first page"
    );
    let expected_page2_first = *page1.last().unwrap() + 1;
    assert_eq!(
        page2[0], expected_page2_first,
        "second page after PageDown should immediately follow the first page"
    );

    let interior_offset = 3usize;
    overlay.view.scroll_offset = interior_offset;
    let before = transcript_line_numbers(&mut overlay, area);
    overlay.view.scroll_offset = overlay.view.scroll_offset.saturating_add(page_height);
    let _ = transcript_line_numbers(&mut overlay, area);
    overlay.view.scroll_offset = overlay.view.scroll_offset.saturating_sub(page_height);
    let after = transcript_line_numbers(&mut overlay, area);
    assert_eq!(
        before, after,
        "PageDown+PageUp from interior offset ({interior_offset}) should round-trip"
    );

    overlay.view.scroll_offset = page_height;
    let before2 = transcript_line_numbers(&mut overlay, area);
    overlay.view.scroll_offset = overlay.view.scroll_offset.saturating_sub(page_height);
    let _ = transcript_line_numbers(&mut overlay, area);
    overlay.view.scroll_offset = overlay.view.scroll_offset.saturating_add(page_height);
    let after2 = transcript_line_numbers(&mut overlay, area);
    assert_eq!(
        before2, after2,
        "PageUp+PageDown from the top of the second page should round-trip"
    );
}

fn static_overlay_wraps_long_lines() {
    let mut overlay = StaticOverlay::with_title(
        vec![
            "a very long line that should wrap when rendered within a narrow pager overlay width"
                .into(),
        ],
        "S T A T I C".to_string(),
    );
    let mut term = Terminal::new(TestBackend::new(24, 8)).expect("term");
    term.draw(|f| overlay.render(f.area(), f.buffer_mut()))
        .expect("draw");
    assert_snapshot!(term.backend());
}

fn pager_view_content_height_counts_renderables() {
    use pager_view::PagerView;
    let pv = PagerView::new(
        vec![paragraph_block("a", 2), paragraph_block("b", 3)],
        "T".to_string(),
        0,
    );

    assert_eq!(pv.content_height(80), 5);
}

fn pager_view_ensure_chunk_visible_scrolls_down_when_needed() {
    use pager_view::PagerView;
    let mut pv = PagerView::new(
        vec![
            paragraph_block("a", 1),
            paragraph_block("b", 3),
            paragraph_block("c", 3),
        ],
        "T".to_string(),
        0,
    );
    let area = Rect::new(0, 0, 20, 8);

    pv.scroll_offset = 0;
    let content_area = pv.content_area(area);
    pv.ensure_chunk_visible(2, content_area);

    let mut buf = Buffer::empty(area);
    pv.render(area, &mut buf);
    let rendered = buffer_to_text(&buf, area);

    assert!(
        rendered.contains("c0"),
        "expected chunk top in view: {rendered:?}"
    );
    assert!(
        rendered.contains("c1"),
        "expected chunk middle in view: {rendered:?}"
    );
    assert!(
        rendered.contains("c2"),
        "expected chunk bottom in view: {rendered:?}"
    );
}

fn pager_view_ensure_chunk_visible_scrolls_up_when_needed() {
    use pager_view::PagerView;
    let mut pv = PagerView::new(
        vec![
            paragraph_block("a", 2),
            paragraph_block("b", 3),
            paragraph_block("c", 3),
        ],
        "T".to_string(),
        0,
    );
    let area = Rect::new(0, 0, 20, 3);

    pv.scroll_offset = 6;
    pv.ensure_chunk_visible(0, area);

    assert_eq!(pv.scroll_offset, 0);
}

fn pager_view_is_scrolled_to_bottom_accounts_for_wrapped_height() {
    use pager_view::PagerView;
    let mut pv = PagerView::new(vec![paragraph_block("a", 10)], "T".to_string(), 0);
    let area = Rect::new(0, 0, 20, 8);
    let mut buf = Buffer::empty(area);

    pv.render(area, &mut buf);

    assert!(
        !pv.is_scrolled_to_bottom(),
        "expected view to report not at bottom when offset < max"
    );

    pv.scroll_offset = usize::MAX;
    pv.render(area, &mut buf);

    assert!(
        pv.is_scrolled_to_bottom(),
        "expected view to report at bottom after scrolling to end"
    );
}

fn pager_view_resolves_bottom_sentinel_to_max_scroll() {
    use pager_view::PagerView;
    let pv = PagerView::new(vec![paragraph_block("a", 10)], "T".to_string(), usize::MAX);
    let area = Rect::new(0, 0, 20, 8);

    let resolved = pv.resolved_scroll_offset_for_area(pv.content_area(area));

    assert!(
        resolved > 0,
        "expected a concrete bottom offset, got {resolved}"
    );
    assert_ne!(resolved, usize::MAX);
}

fn pager_view_mouse_scroll_moves_relative_to_current_position() {
    use crossterm::event::MouseEventKind;
    use pager_view::PagerView;
    let mut pv = PagerView::new(vec![paragraph_block("a", 30)], "T".to_string(), 9);
    let area = Rect::new(0, 0, 20, 8);

    assert!(pv.apply_mouse_scroll(MouseEventKind::ScrollUp, area));
    assert_eq!(pv.scroll_offset, 6);

    assert!(pv.apply_mouse_scroll(MouseEventKind::ScrollDown, area));
    assert_eq!(pv.scroll_offset, 9);
}

fn pager_view_mouse_scroll_resolves_bottom_sentinel_before_scrolling_up() {
    use crossterm::event::MouseEventKind;
    use pager_view::PagerView;
    let mut pv = PagerView::new(vec![paragraph_block("a", 20)], "T".to_string(), usize::MAX);
    let area = Rect::new(0, 0, 20, 7);

    assert!(pv.apply_mouse_scroll(MouseEventKind::ScrollUp, area));
    assert_eq!(pv.scroll_offset, 12);
}
