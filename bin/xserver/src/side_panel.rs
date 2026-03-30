use std::time::{Duration, Instant};

use jiff::Timestamp;
use chaos_ipc::ProcessId;
use chaos_proc::{LogRow, LogTailBatch, LogTailCursor};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::Widget;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

pub(crate) const LOG_PANEL_POLL_INTERVAL: Duration = Duration::from_millis(500);
pub(crate) const LOG_PANEL_BACKFILL_LIMIT: usize = 200;

const LOG_PANEL_MIN_WIDTH: u16 = 28;
const LOG_PANEL_MAX_WIDTH: u16 = 52;
const LOG_PANEL_MAIN_MIN_WIDTH: u16 = 40;
const LOG_PANEL_EMPTY_MESSAGE: &str = "No logs yet for this session.";
const LOG_PANEL_WAITING_MESSAGE: &str = "Waiting for a session before showing logs.";

#[derive(Debug, Clone)]
pub(crate) struct LogPanelState {
    visible: bool,
    rows: Vec<LogRow>,
    cursor: LogTailCursor,
    process_id: Option<ProcessId>,
    error_message: Option<String>,
    viewport_height: u16,
    scroll_offset: u16,
    follow: bool,
    next_poll_at: Option<Instant>,
}

impl Default for LogPanelState {
    fn default() -> Self {
        Self {
            visible: false,
            rows: Vec::new(),
            cursor: LogTailCursor::default(),
            process_id: None,
            error_message: None,
            viewport_height: 0,
            scroll_offset: 0,
            follow: true,
            next_poll_at: None,
        }
    }
}

impl LogPanelState {
    pub(crate) fn is_visible(&self) -> bool {
        self.visible
    }

    pub(crate) fn toggle(&mut self) -> bool {
        self.visible = !self.visible;
        if self.visible {
            self.follow = true;
            self.next_poll_at = None;
        }
        self.visible
    }

    pub(crate) fn process_id(&self) -> Option<ProcessId> {
        self.process_id
    }

    pub(crate) fn cursor(&self) -> LogTailCursor {
        self.cursor.clone()
    }

    pub(crate) fn set_process_id(&mut self, process_id: Option<ProcessId>) -> bool {
        if self.process_id == process_id {
            return false;
        }
        self.process_id = process_id;
        self.rows.clear();
        self.cursor = LogTailCursor::default();
        self.error_message = None;
        self.scroll_offset = 0;
        self.follow = true;
        self.next_poll_at = None;
        true
    }

    pub(crate) fn set_viewport_height(&mut self, viewport_height: u16) {
        self.viewport_height = viewport_height;
        self.clamp_scroll();
    }

    pub(crate) fn should_poll(&self, now: Instant) -> bool {
        self.visible && self.next_poll_at.is_none_or(|deadline| now >= deadline)
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
        self.scroll_to_end_if_following();
    }

    pub(crate) fn append_batch(&mut self, batch: LogTailBatch) {
        if !batch.rows.is_empty() {
            self.rows.extend(batch.rows);
        }
        self.cursor = batch.cursor;
        self.error_message = None;
        self.scroll_to_end_if_following();
    }

    pub(crate) fn scroll_up(&mut self, amount: u16) {
        self.follow = false;
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
    }

    pub(crate) fn scroll_down(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(amount);
        self.clamp_scroll();
        self.follow = self.scroll_offset >= self.max_scroll();
    }

    pub(crate) fn scroll_to_start(&mut self) {
        self.follow = false;
        self.scroll_offset = 0;
    }

    pub(crate) fn scroll_to_end(&mut self) {
        self.follow = true;
        self.scroll_offset = self.max_scroll();
    }

    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default().borders(Borders::ALL).title(
            format!(
                " Logs {} ",
                self.process_id
                    .map(|id| short_process_id(id))
                    .unwrap_or_else(|| "idle".to_string())
            )
            .bold(),
        );
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let paragraph = Paragraph::new(self.render_lines())
            .style(Style::default())
            .scroll((self.scroll_offset.min(self.max_scroll()), 0));
        paragraph.render(inner, buf);
    }

    fn render_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(self.status_line());
        if let Some(error) = self.error_message.as_ref() {
            lines.push(Line::from(error.clone()).style(Style::default().fg(Color::Red)));
            return lines;
        }

        if self.process_id.is_none() {
            lines.push(Line::from(LOG_PANEL_WAITING_MESSAGE).style(Style::default().fg(Color::DarkGray)));
            return lines;
        }

        if self.rows.is_empty() {
            lines.push(Line::from(LOG_PANEL_EMPTY_MESSAGE).style(Style::default().fg(Color::DarkGray)));
            return lines;
        }

        lines.extend(self.rows.iter().map(render_log_row));
        lines
    }

    fn status_line(&self) -> Line<'static> {
        let follow_label = if self.follow { "follow" } else { "paused" };
        let count = self.rows.len();
        Line::from(format!("{follow_label} • {count} rows • Ctrl+O close"))
            .style(Style::default().fg(Color::DarkGray))
    }

    fn max_scroll(&self) -> u16 {
        let total_lines = self.render_lines().len() as u16;
        total_lines.saturating_sub(self.viewport_height.max(1))
    }

    fn clamp_scroll(&mut self) {
        self.scroll_offset = self.scroll_offset.min(self.max_scroll());
    }

    fn scroll_to_end_if_following(&mut self) {
        if self.follow {
            self.scroll_offset = self.max_scroll();
        } else {
            self.clamp_scroll();
        }
    }
}

pub(crate) fn split_main_and_panel(area: Rect, panel_visible: bool) -> (Rect, Option<Rect>) {
    if !panel_visible {
        return (area, None);
    }

    if area.width < LOG_PANEL_MIN_WIDTH + LOG_PANEL_MAIN_MIN_WIDTH {
        return (area, None);
    }

    let ideal = ((area.width as f32) * 0.35).round() as u16;
    let panel_width = ideal
        .max(LOG_PANEL_MIN_WIDTH)
        .min(LOG_PANEL_MAX_WIDTH)
        .min(area.width.saturating_sub(LOG_PANEL_MAIN_MIN_WIDTH));

    let [main, panel] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(area.width.saturating_sub(panel_width)),
            Constraint::Length(panel_width),
        ])
        .areas(area);
    (main, Some(panel))
}

fn render_log_row(row: &LogRow) -> Line<'static> {
    let timestamp = format_timestamp(row.ts, row.ts_nanos);
    let level = format!("{:<5}", row.level);
    let target = row.target.clone();
    let message = row.message.clone().unwrap_or_default();
    let process_marker = row
        .process_id
        .as_ref()
        .map(|_| "")
        .unwrap_or(" *");
    Line::from(format!("{timestamp} {level} {target}{process_marker} {message}"))
}

fn format_timestamp(ts: i64, ts_nanos: i64) -> String {
    let _ = ts_nanos;
    Timestamp::from_second(ts)
        .ok()
        .map(|dt| dt.to_zoned(jiff::tz::TimeZone::UTC).strftime("%H:%M:%S").to_string())
        .unwrap_or_else(|| format!("{ts}"))
}

fn short_process_id(process_id: ProcessId) -> String {
    process_id
        .to_string()
        .chars()
        .take(8)
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_backend::VT100Backend;
    use insta::assert_snapshot;

    fn fixed_process_id() -> ProcessId {
        ProcessId::from_string("019d40be-0000-0000-0000-000000000000")
            .expect("fixed process id")
    }

    fn sample_row(id: i64, ts: i64, message: &str) -> LogRow {
        LogRow {
            id,
            ts,
            ts_nanos: 0,
            level: "INFO".to_string(),
            target: "chaos::test".to_string(),
            message: Some(message.to_string()),
            process_id: Some(fixed_process_id().to_string()),
            process_uuid: None,
            file: None,
            line: None,
        }
    }

    #[test]
    fn split_main_and_panel_hides_panel_when_terminal_too_narrow() {
        let area = Rect::new(0, 0, 50, 20);
        let (main, panel) = split_main_and_panel(area, true);
        assert_eq!(main, area);
        assert_eq!(panel, None);
    }

    #[test]
    fn scroll_follow_state_changes_as_expected() {
        let mut panel = LogPanelState {
            visible: true,
            process_id: Some(fixed_process_id()),
            rows: (0..10)
                .map(|idx| sample_row(idx, 1_700_000_000 + idx, &format!("line {idx}")))
                .collect(),
            viewport_height: 4,
            ..Default::default()
        };
        panel.scroll_to_end();
        let end_scroll = panel.scroll_offset;
        assert!(panel.follow);

        panel.scroll_up(2);
        assert!(!panel.follow);
        assert!(panel.scroll_offset < end_scroll);

        panel.scroll_to_end();
        assert!(panel.follow);
        assert_eq!(panel.scroll_offset, panel.max_scroll());
    }

    #[test]
    fn render_snapshot_with_logs() {
        let backend = VT100Backend::new(42, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        let mut panel = LogPanelState {
            visible: true,
            process_id: Some(fixed_process_id()),
            rows: vec![
                sample_row(1, 1_700_000_000, "boot"),
                sample_row(2, 1_700_000_001, "tick"),
                sample_row(3, 1_700_000_002, "ready"),
            ],
            ..Default::default()
        };
        panel.set_viewport_height(10);

        terminal
            .draw(|frame| {
                panel.render(frame.area(), frame.buffer_mut());
            })
            .expect("draw panel");

        assert_snapshot!(terminal.backend().to_string());
    }
}
