use std::time::Duration;
use std::time::Instant;

use chaos_ipc::ProcessId;
use chaos_proc::LogRow;
use chaos_proc::LogTailBatch;
use chaos_proc::LogTailCursor;
use jiff::Timestamp;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;

pub(crate) const LOG_PANEL_POLL_INTERVAL: Duration = Duration::from_millis(500);
pub(crate) const LOG_PANEL_BACKFILL_LIMIT: usize = 200;

const LOG_PANEL_EMPTY_MESSAGE: &str = "No logs yet for this session.";
const LOG_PANEL_WAITING_MESSAGE: &str = "Waiting for a session before showing logs.";

/// Data buffer for session logs. Feeds the full-screen log overlay opened by
/// Ctrl+O. No rendering happens here — the overlay handles display.
#[derive(Debug, Default)]
pub(crate) struct LogPanelState {
    process_id: Option<ProcessId>,
    rows: Vec<LogRow>,
    cursor: LogTailCursor,
    error_message: Option<String>,
    next_poll_at: Option<Instant>,
}

impl LogPanelState {
    pub(crate) fn process_id(&self) -> Option<ProcessId> {
        self.process_id
    }

    /// Updates the tracked process id. Returns `true` if the id changed
    /// (caller should trigger a backfill).
    pub(crate) fn set_process_id(&mut self, process_id: Option<ProcessId>) -> bool {
        if self.process_id == process_id {
            return false;
        }
        self.process_id = process_id;
        self.rows.clear();
        self.cursor = LogTailCursor::default();
        self.error_message = None;
        self.next_poll_at = None;
        true
    }

    pub(crate) fn schedule_next_poll(&mut self, now: Instant) {
        self.next_poll_at = Some(now + LOG_PANEL_POLL_INTERVAL);
    }

    pub(crate) fn set_error(&mut self, message: String) {
        self.error_message = Some(message);
        self.next_poll_at = None;
    }

    pub(crate) fn replace_batch(&mut self, batch: LogTailBatch) {
        self.rows = batch.rows;
        self.cursor = batch.cursor;
        self.error_message = None;
    }

    /// Snapshot the current log state as styled lines for the pager overlay.
    pub(crate) fn render_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        let count = self.rows.len();
        lines.push(
            Line::from(format!("{count} rows • q to close"))
                .style(Style::default().fg(Color::DarkGray)),
        );

        if let Some(error) = self.error_message.as_ref() {
            lines.push(Line::from(error.clone()).style(Style::default().fg(Color::Red)));
            return lines;
        }

        if self.process_id.is_none() {
            lines.push(
                Line::from(LOG_PANEL_WAITING_MESSAGE).style(Style::default().fg(Color::DarkGray)),
            );
            return lines;
        }

        if self.rows.is_empty() {
            lines.push(
                Line::from(LOG_PANEL_EMPTY_MESSAGE).style(Style::default().fg(Color::DarkGray)),
            );
            return lines;
        }

        lines.extend(self.rows.iter().map(render_log_row));
        lines
    }
}

fn render_log_row(row: &LogRow) -> Line<'static> {
    let timestamp = format_timestamp(row.ts, row.ts_nanos);
    let level = format!("{:<5}", row.level);
    let target = row.target.clone();
    let message = row.message.clone().unwrap_or_default();
    let process_marker = row.process_id.as_ref().map(|_| "").unwrap_or(" *");
    Line::from(format!(
        "{timestamp} {level} {target}{process_marker} {message}"
    ))
}

fn format_timestamp(ts: i64, ts_nanos: i64) -> String {
    let _ = ts_nanos;
    Timestamp::from_second(ts)
        .ok()
        .map(|dt| {
            dt.to_zoned(jiff::tz::TimeZone::UTC)
                .strftime("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| format!("{ts}"))
}
