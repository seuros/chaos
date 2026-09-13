use super::*;
use pretty_assertions::assert_eq;

pub(crate) fn transcript_reflow_suite() {
    first_width_only_establishes_a_baseline();
    rebuild_tracks_the_rendered_width_not_the_observed_one();
    repeated_resizes_push_the_deadline_out();
    row_caps_follow_the_detected_terminal();
    rebuilt_rows_are_spaced_and_capped_like_live_inserts();
}

/// A cell whose rendering depends on the width it is asked for, so a rebuild at a new width is
/// distinguishable from a replay of the rows written at the old one.
#[derive(Debug)]
struct StubCell {
    label: &'static str,
    rows: usize,
    is_continuation: bool,
}

impl StubCell {
    fn cell(label: &'static str, rows: usize, is_continuation: bool) -> Arc<dyn HistoryCell> {
        Arc::new(Self {
            label,
            rows,
            is_continuation,
        })
    }
}

impl HistoryCell for StubCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        (0..self.rows)
            .map(|row| Line::from(format!("{}{row}@{width}", self.label)))
            .collect()
    }

    fn is_stream_continuation(&self) -> bool {
        self.is_continuation
    }
}

fn texts(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

/// Separators go between top-level cells and never inside a stream run; the cap keeps the
/// newest rows and still drags in the cell that opened a truncated run.
fn rebuilt_rows_are_spaced_and_capped_like_live_inserts() {
    let cells = vec![
        StubCell::cell("a", 1, false),
        StubCell::cell("b", 1, false),
        StubCell::cell("c", 1, true),
    ];

    assert_eq!(
        texts(&reflow_transcript_lines(&cells, 40, None)),
        vec!["a0@40", "", "b0@40", "c0@40"],
        "the first cell takes no leading blank and a continuation takes none at all"
    );

    // Two rows of budget can only hold the tail, but the run's opening cell is still rendered
    // so the continuation does not get promoted into a separated cell of its own.
    assert_eq!(
        texts(&reflow_transcript_lines(&cells, 72, Some(2))),
        vec!["b0@72", "c0@72"]
    );

    assert!(reflow_transcript_lines(&[], 72, None).is_empty());
}

/// Nothing has been written at another width on the first draw, so no repair is owed.
fn first_width_only_establishes_a_baseline() {
    let mut state = TranscriptReflowState::default();

    let change = state.note_width(80);

    assert!(change.initialized);
    assert!(!change.changed);
    assert!(!state.reflow_needed_for_width(80));
    assert!(state.reflow_needed_for_width(100));
    assert_eq!(state.pending_until(), None);
}

/// A terminal that settles on its final width after the rebuild still gets repaired: the
/// observed-width tracker has seen 100, but nothing has rendered at 100 yet.
fn rebuild_tracks_the_rendered_width_not_the_observed_one() {
    let mut state = TranscriptReflowState::default();
    state.note_width(80);
    state.schedule_debounced(Some(100));
    assert!(!state.reflow_needed_for_width(100));

    state.mark_reflowed_width(90);
    state.clear_pending_reflow();
    state.note_width(100);

    assert!(state.reflow_needed_for_width(100));
    assert_eq!(state.pending_until(), None);

    state.clear();
    assert!(state.note_width(100).initialized);
}

/// A drag must rebuild once, at the width the user released on.
fn repeated_resizes_push_the_deadline_out() {
    let mut state = TranscriptReflowState::default();
    state.note_width(80);

    state.schedule_debounced(Some(90));
    let first_deadline = state.pending_until().expect("a rebuild is queued");
    assert!(!state.pending_is_due(Instant::now()));

    std::thread::sleep(Duration::from_millis(2));
    state.schedule_debounced(Some(100));

    assert!(state.pending_until().expect("a rebuild is queued") > first_deadline);
    assert!(state.reflow_needed_for_width(90));
    assert!(!state.reflow_needed_for_width(100));
    assert!(state.pending_is_due(first_deadline + TRANSCRIPT_REFLOW_DEBOUNCE));
}

/// Caps come from the terminal's own scrollback default; explicit settings override it.
fn row_caps_follow_the_detected_terminal() {
    assert_eq!(auto_max_rows(TerminalName::VsCode), VSCODE_REFLOW_MAX_ROWS);
    assert_eq!(
        auto_max_rows(TerminalName::WezTerm),
        WEZTERM_REFLOW_MAX_ROWS
    );
    assert_eq!(
        auto_max_rows(TerminalName::Alacritty),
        ALACRITTY_REFLOW_MAX_ROWS
    );
    assert_eq!(
        auto_max_rows(TerminalName::Ghostty),
        FALLBACK_REFLOW_MAX_ROWS
    );
    assert_eq!(
        auto_max_rows(TerminalName::Unknown),
        FALLBACK_REFLOW_MAX_ROWS
    );

    assert_eq!(ReflowRowCap::Unlimited.max_rows(), None);
    assert_eq!(ReflowRowCap::Limit(42).max_rows(), Some(42));
    assert!(ReflowRowCap::default().max_rows().is_some());
}
