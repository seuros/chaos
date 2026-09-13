mod formatting;
mod rendering;
mod row;
mod state;

use std::sync::Arc;

use chaos_ipc::ProcessId;
use chaos_kern::INTERACTIVE_SESSION_SOURCES;
use chaos_kern::ProcessSortKey;
use chaos_kern::RolloutRecorder;
use chaos_kern::config::Config;
use color_eyre::eyre::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::style::Stylize as _;
use ratatui::text::Line;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::key_hint;
use crate::tui::Tui;
use crate::tui::TuiEvent;

use self::rendering::calculate_column_metrics;
use self::rendering::render_column_headers;
use self::rendering::render_list;
use self::rendering::search_line;
use self::state::BackgroundEvent;
use self::state::PageLoadRequest;
use self::state::PickerState;

#[derive(Debug, Clone)]
pub struct SessionTarget {
    pub process_id: ProcessId,
    pub keep_current: bool,
}

impl SessionTarget {
    /// Apply the picker's automatic restoration without changing persisted defaults.
    pub(crate) async fn apply_saved_selection(
        &self,
        config: &mut Config,
        overrides: &[(String, toml::Value)],
    ) -> Result<Option<String>> {
        let mut intent =
            chaos_kern::saved_selection::RestoreSelection::from_cli_overrides(overrides);
        intent.keep_current |= self.keep_current;
        chaos_kern::saved_selection::restore_process_selection(config, self.process_id, intent)
            .await
            .map_err(|err| color_eyre::eyre::eyre!("{err:#}"))
    }
}

#[derive(Debug, Clone)]
pub enum SessionSelection {
    StartFresh,
    Resume(SessionTarget),
    Fork(SessionTarget),
    Exit,
}

#[derive(Clone, Copy, Debug)]
pub enum SessionPickerAction {
    Resume,
    Fork,
}

impl SessionPickerAction {
    fn title(self) -> &'static str {
        match self {
            SessionPickerAction::Resume => "Resume a previous session",
            SessionPickerAction::Fork => "Fork a previous session",
        }
    }

    pub(crate) fn action_label(self) -> &'static str {
        match self {
            SessionPickerAction::Resume => "resume",
            SessionPickerAction::Fork => "fork",
        }
    }

    pub(crate) fn selection(self, process_id: ProcessId, keep_current: bool) -> SessionSelection {
        let target_session = SessionTarget {
            process_id,
            keep_current,
        };
        match self {
            SessionPickerAction::Resume => SessionSelection::Resume(target_session),
            SessionPickerAction::Fork => SessionSelection::Fork(target_session),
        }
    }
}

/// Returns the human-readable column header for the given sort key.
fn sort_key_label(sort_key: ProcessSortKey) -> &'static str {
    match sort_key {
        ProcessSortKey::CreatedAt => "Created at",
        ProcessSortKey::UpdatedAt => "Updated at",
    }
}

/// RAII guard that ensures we leave the alt-screen on scope exit.
struct AltScreenGuard<'a> {
    tui: &'a mut Tui,
}

impl<'a> AltScreenGuard<'a> {
    fn enter(tui: &'a mut Tui) -> Self {
        let _ = tui.enter_alt_screen();
        Self { tui }
    }
}

impl Drop for AltScreenGuard<'_> {
    fn drop(&mut self) {
        let _ = self.tui.leave_alt_screen();
    }
}

/// Interactive session picker that lists persisted sessions with simple
/// search and pagination.
///
/// The picker displays sessions in a table with timestamp columns (created/updated),
/// git branch, working directory, and conversation preview. Users can toggle
/// between sorting by creation time and last-updated time using Shift+Tab.
/// Selected-session details show the saved provider and cumulative token usage,
/// Opening restores the last-used provider, model and effort. Tab
/// toggles keeping the current model instead, without a confirmation dialog.
///
/// Sessions are loaded on-demand via cursor-based pagination. The backend
/// `RolloutRecorder::list_processes` returns pages ordered by the selected sort key,
/// and the picker deduplicates across pages to handle overlapping windows when
/// new sessions appear during pagination.
///
/// Filtering happens in two layers:
/// 1. Source filtering at the backend (interactive CLI sessions across providers).
/// 2. Working-directory filtering at the picker (unless `--all` is passed).
pub async fn run_resume_picker(
    tui: &mut Tui,
    config: &Config,
    show_all: bool,
) -> Result<SessionSelection> {
    run_session_picker(tui, config, show_all, SessionPickerAction::Resume).await
}

pub(crate) async fn run_session_picker(
    tui: &mut Tui,
    config: &Config,
    show_all: bool,
    action: SessionPickerAction,
) -> Result<SessionSelection> {
    let alt = AltScreenGuard::enter(tui);
    let (bg_tx, bg_rx) = mpsc::unbounded_channel();

    let default_provider = config.model_provider_id.to_string();
    let filter_cwd = if show_all {
        None
    } else {
        std::env::current_dir().ok()
    };

    let config = config.clone();
    let loader_tx = bg_tx.clone();
    let page_loader = Arc::new(move |request: PageLoadRequest| {
        let tx = loader_tx.clone();
        let config = config.clone();
        tokio::spawn(async move {
            // No provider filter: show sessions from all providers so that
            // switching between profiles (e.g. openai ↔ xai) doesn't hide
            // sessions started with a different provider.
            let page = RolloutRecorder::list_processes(
                &config,
                state::PAGE_SIZE,
                request.cursor.as_ref(),
                request.sort_key,
                INTERACTIVE_SESSION_SOURCES,
                request.default_provider.as_str(),
                /*search_term*/ None,
            )
            .await;
            let _ = tx.send(BackgroundEvent::PageLoaded {
                request_token: request.request_token,
                search_token: request.search_token,
                page,
            });
        });
    });

    let mut state = PickerState::new(
        alt.tui.frame_requester(),
        page_loader,
        default_provider.clone(),
        show_all,
        filter_cwd,
        action,
    );
    state.start_initial_load();
    state.request_frame();

    let mut tui_events = alt.tui.event_stream().fuse();
    let mut background_events = UnboundedReceiverStream::new(bg_rx).fuse();
    let mut requested_selections = std::collections::HashSet::new();

    loop {
        // Load only highlighted histories, never every journal in a page.
        if let Some(row) = state.filtered_rows.get(state.selected)
            && requested_selections.insert(row.process_id)
        {
            let process_id = row.process_id;
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let selection = chaos_kern::saved_selection::load_saved_selection(process_id)
                    .await
                    .map_err(|err| err.to_string());
                let _ = tx.send(BackgroundEvent::SelectionLoaded {
                    process_id,
                    selection,
                });
            });
        }
        tokio::select! {
            Some(ev) = tui_events.next() => {
                match ev {
                    TuiEvent::Key(key) => {
                        if matches!(key.kind, KeyEventKind::Release) {
                            continue;
                        }
                        if let Some(sel) = state.handle_key(key).await? {
                            return Ok(sel);
                        }
                    }
                    TuiEvent::Draw => {
                        if let Ok(size) = alt.tui.terminal.size() {
                            let list_height = size.height.saturating_sub(6) as usize;
                            state.update_view_rows(list_height);
                            state.ensure_minimum_rows_for_view(list_height);
                        }
                        draw_picker(alt.tui, &state)?;
                    }
                    _ => {}
                }
            }
            Some(event) = background_events.next() => {
                state.handle_background_event(event).await?;
            }
            else => break,
        }
    }

    // Fallback – treat as cancel/new
    Ok(SessionSelection::StartFresh)
}

fn draw_picker(tui: &mut Tui, state: &PickerState) -> std::io::Result<()> {
    let height = tui.terminal.size()?.height;
    tui.draw(height, |frame| {
        let area = frame.area();
        let [header, search, columns, list, details, notice, hint] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);

        // Header
        let header_line: Line = vec![
            state.action.title().bold().cyan(),
            "  ".into(),
            "Sort:".dim(),
            " ".into(),
            sort_key_label(state.sort_key).magenta(),
        ]
        .into();
        frame.render_widget(header_line, header);

        // Search line
        frame.render_widget(search_line(state), search);

        let metrics = calculate_column_metrics(&state.filtered_rows, state.show_all);

        // Column headers and list
        render_column_headers(frame, columns, &metrics, state.sort_key);
        render_list(frame, list, state, &metrics);
        let [details_line, notice_line] = rendering::selected_details(state);
        frame.render_widget(details_line, details);
        frame.render_widget(notice_line, notice);

        // Hint line
        let action_label = state.action.action_label();
        let hint_line: Line = vec![
            key_hint::plain(KeyCode::Enter).into(),
            format!(" to {action_label} ").dim(),
            "    ".dim(),
            key_hint::plain(KeyCode::Esc).into(),
            " to start new ".dim(),
            "    ".dim(),
            key_hint::ctrl(KeyCode::Char('c')).into(),
            " to quit ".dim(),
            "    ".dim(),
            "Shift+Tab".into(),
            " to toggle sort ".dim(),
            "    ".dim(),
            key_hint::plain(KeyCode::Up).into(),
            "/".dim(),
            key_hint::plain(KeyCode::Down).into(),
            " to browse".dim(),
        ]
        .into();
        frame.render_widget(hint_line, hint);
    })
}

#[cfg(test)]
pub(crate) mod tests;
