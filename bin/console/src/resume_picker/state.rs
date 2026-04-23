use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use chaos_ipc::ProcessId;
use chaos_kern::Cursor;
use chaos_kern::ProcessSortKey;
use chaos_kern::ProcessesPage;
use chaos_kern::find_process_names_by_ids;
use color_eyre::eyre::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;

use crate::tui::FrameRequester;

use super::row::Row;
use super::row::paths_match;
use super::row::rows_from_items;

use super::SessionPickerAction;
use super::SessionSelection;

pub(super) const PAGE_SIZE: usize = 25;
pub(super) const LOAD_NEAR_THRESHOLD: usize = 5;

#[derive(Clone)]
pub(super) struct PageLoadRequest {
    pub(super) cursor: Option<Cursor>,
    pub(super) request_token: usize,
    pub(super) search_token: Option<usize>,
    pub(super) default_provider: String,
    pub(super) sort_key: ProcessSortKey,
}

pub(super) type PageLoader = Arc<dyn Fn(PageLoadRequest) + Send + Sync>;

pub(super) enum BackgroundEvent {
    PageLoaded {
        request_token: usize,
        search_token: Option<usize>,
        page: std::io::Result<ProcessesPage>,
    },
}

pub(super) struct PaginationState {
    pub(super) next_cursor: Option<Cursor>,
    pub(super) num_scanned_records: usize,
    pub(super) reached_scan_limit: bool,
    pub(super) loading: LoadingState,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum LoadingState {
    Idle,
    Pending(PendingLoad),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PendingLoad {
    pub(super) request_token: usize,
    pub(super) search_token: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum SearchState {
    Idle,
    Active { token: usize },
}

pub(super) enum LoadTrigger {
    Scroll,
    Search { token: usize },
}

impl LoadingState {
    pub(super) fn is_pending(&self) -> bool {
        matches!(self, LoadingState::Pending(_))
    }
}

impl SearchState {
    pub(super) fn active_token(&self) -> Option<usize> {
        match self {
            SearchState::Idle => None,
            SearchState::Active { token } => Some(*token),
        }
    }

    pub(super) fn is_active(&self) -> bool {
        self.active_token().is_some()
    }
}

pub(super) struct PickerState {
    pub(super) chaos_home: PathBuf,
    pub(super) requester: FrameRequester,
    pub(super) pagination: PaginationState,
    pub(super) all_rows: Vec<Row>,
    pub(super) filtered_rows: Vec<Row>,
    pub(super) seen_process_ids: HashSet<ProcessId>,
    pub(super) selected: usize,
    pub(super) scroll_top: usize,
    pub(super) query: String,
    pub(super) search_state: SearchState,
    pub(super) next_request_token: usize,
    pub(super) next_search_token: usize,
    pub(super) page_loader: PageLoader,
    pub(super) view_rows: Option<usize>,
    pub(super) default_provider: String,
    pub(super) show_all: bool,
    pub(super) filter_cwd: Option<PathBuf>,
    pub(super) action: SessionPickerAction,
    pub(super) sort_key: ProcessSortKey,
    pub(super) process_name_cache: HashMap<ProcessId, Option<String>>,
    pub(super) inline_error: Option<String>,
}

impl PickerState {
    pub(super) fn new(
        chaos_home: PathBuf,
        requester: FrameRequester,
        page_loader: PageLoader,
        default_provider: String,
        show_all: bool,
        filter_cwd: Option<PathBuf>,
        action: SessionPickerAction,
    ) -> Self {
        Self {
            chaos_home,
            requester,
            pagination: PaginationState {
                next_cursor: None,
                num_scanned_records: 0,
                reached_scan_limit: false,
                loading: LoadingState::Idle,
            },
            all_rows: Vec::new(),
            filtered_rows: Vec::new(),
            seen_process_ids: HashSet::new(),
            selected: 0,
            scroll_top: 0,
            query: String::new(),
            search_state: SearchState::Idle,
            next_request_token: 0,
            next_search_token: 0,
            page_loader,
            view_rows: None,
            default_provider,
            show_all,
            filter_cwd,
            action,
            sort_key: ProcessSortKey::UpdatedAt,
            process_name_cache: HashMap::new(),
            inline_error: None,
        }
    }

    pub(super) fn request_frame(&self) {
        self.requester.schedule_frame();
    }

    pub(super) async fn handle_key(&mut self, key: KeyEvent) -> Result<Option<SessionSelection>> {
        self.inline_error = None;
        match key.code {
            KeyCode::Esc => return Ok(Some(SessionSelection::StartFresh)),
            KeyCode::Char('c')
                if key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                return Ok(Some(SessionSelection::Exit));
            }
            KeyCode::Enter => {
                if let Some(row) = self.filtered_rows.get(self.selected) {
                    return Ok(Some(self.action.selection(row.process_id)));
                }
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.ensure_selected_visible();
                }
                self.request_frame();
            }
            KeyCode::Down => {
                if self.selected + 1 < self.filtered_rows.len() {
                    self.selected += 1;
                    self.ensure_selected_visible();
                }
                self.maybe_load_more_for_scroll();
                self.request_frame();
            }
            KeyCode::PageUp => {
                let step = self.view_rows.unwrap_or(10).max(1);
                if self.selected > 0 {
                    self.selected = self.selected.saturating_sub(step);
                    self.ensure_selected_visible();
                    self.request_frame();
                }
            }
            KeyCode::PageDown if !self.filtered_rows.is_empty() => {
                let step = self.view_rows.unwrap_or(10).max(1);
                let max_index = self.filtered_rows.len().saturating_sub(1);
                self.selected = (self.selected + step).min(max_index);
                self.ensure_selected_visible();
                self.maybe_load_more_for_scroll();
                self.request_frame();
            }
            KeyCode::Tab => {
                self.toggle_sort_key();
                self.request_frame();
            }
            KeyCode::Backspace => {
                let mut new_query = self.query.clone();
                new_query.pop();
                self.set_query(new_query);
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL)
                    && !key.modifiers.contains(crossterm::event::KeyModifiers::ALT) =>
            {
                let mut new_query = self.query.clone();
                new_query.push(c);
                self.set_query(new_query);
            }
            _ => {}
        }
        Ok(None)
    }

    pub(super) fn start_initial_load(&mut self) {
        self.reset_pagination();
        self.all_rows.clear();
        self.filtered_rows.clear();
        self.seen_process_ids.clear();
        self.selected = 0;

        let search_token = if self.query.is_empty() {
            self.search_state = SearchState::Idle;
            None
        } else {
            let token = self.allocate_search_token();
            self.search_state = SearchState::Active { token };
            Some(token)
        };

        let request_token = self.allocate_request_token();
        self.pagination.loading = LoadingState::Pending(PendingLoad {
            request_token,
            search_token,
        });
        self.request_frame();

        (self.page_loader)(PageLoadRequest {
            cursor: None,
            request_token,
            search_token,
            default_provider: self.default_provider.clone(),
            sort_key: self.sort_key,
        });
    }

    pub(super) async fn handle_background_event(&mut self, event: BackgroundEvent) -> Result<()> {
        match event {
            BackgroundEvent::PageLoaded {
                request_token,
                search_token,
                page,
            } => {
                let pending = match self.pagination.loading {
                    LoadingState::Pending(pending) => pending,
                    LoadingState::Idle => return Ok(()),
                };
                if pending.request_token != request_token {
                    return Ok(());
                }
                self.pagination.loading = LoadingState::Idle;
                let page = page.map_err(color_eyre::Report::from)?;
                self.ingest_page(page);
                self.update_process_names().await;
                let completed_token = pending.search_token.or(search_token);
                self.continue_search_if_token_matches(completed_token);
            }
        }
        Ok(())
    }

    pub(super) fn reset_pagination(&mut self) {
        self.pagination.next_cursor = None;
        self.pagination.num_scanned_records = 0;
        self.pagination.reached_scan_limit = false;
        self.pagination.loading = LoadingState::Idle;
    }

    pub(super) fn ingest_page(&mut self, page: ProcessesPage) {
        if let Some(cursor) = page.next_cursor.clone() {
            self.pagination.next_cursor = Some(cursor);
        } else {
            self.pagination.next_cursor = None;
        }
        self.pagination.num_scanned_records = self
            .pagination
            .num_scanned_records
            .saturating_add(page.num_scanned_records);
        if page.reached_scan_limit {
            self.pagination.reached_scan_limit = true;
        }

        let rows = rows_from_items(page.items);
        for row in rows {
            if self.seen_process_ids.insert(row.process_id) {
                self.all_rows.push(row);
            }
        }

        self.apply_filter();
    }

    async fn update_process_names(&mut self) {
        let mut missing_ids = HashSet::new();
        for row in &self.all_rows {
            let process_id = row.process_id;
            if self.process_name_cache.contains_key(&process_id) {
                continue;
            }
            missing_ids.insert(process_id);
        }

        if missing_ids.is_empty() {
            return;
        }

        let names = find_process_names_by_ids(&self.chaos_home, &missing_ids)
            .await
            .unwrap_or_default();
        for process_id in missing_ids {
            let process_name = names.get(&process_id).cloned();
            self.process_name_cache.insert(process_id, process_name);
        }

        let mut updated = false;
        for row in self.all_rows.iter_mut() {
            let process_id = row.process_id;
            let process_name = self.process_name_cache.get(&process_id).cloned().flatten();
            if row.process_name == process_name {
                continue;
            }
            row.process_name = process_name;
            updated = true;
        }

        if updated {
            self.apply_filter();
        }
    }

    pub(super) fn apply_filter(&mut self) {
        let base_iter = self
            .all_rows
            .iter()
            .filter(|row| self.row_matches_filter(row));
        if self.query.is_empty() {
            self.filtered_rows = base_iter.cloned().collect();
        } else {
            let q = self.query.to_lowercase();
            self.filtered_rows = base_iter.filter(|r| r.matches_query(&q)).cloned().collect();
        }
        if self.selected >= self.filtered_rows.len() {
            self.selected = self.filtered_rows.len().saturating_sub(1);
        }
        if self.filtered_rows.is_empty() {
            self.scroll_top = 0;
        }
        self.ensure_selected_visible();
        self.request_frame();
    }

    fn row_matches_filter(&self, row: &Row) -> bool {
        if self.show_all {
            return true;
        }
        let Some(filter_cwd) = self.filter_cwd.as_ref() else {
            return true;
        };
        let Some(row_cwd) = row.cwd.as_ref() else {
            return false;
        };
        paths_match(row_cwd, filter_cwd)
    }

    pub(super) fn set_query(&mut self, new_query: String) {
        if self.query == new_query {
            return;
        }
        self.query = new_query;
        self.selected = 0;
        self.apply_filter();
        if self.query.is_empty() {
            self.search_state = SearchState::Idle;
            return;
        }
        if !self.filtered_rows.is_empty() {
            self.search_state = SearchState::Idle;
            return;
        }
        if self.pagination.reached_scan_limit || self.pagination.next_cursor.is_none() {
            self.search_state = SearchState::Idle;
            return;
        }
        let token = self.allocate_search_token();
        self.search_state = SearchState::Active { token };
        self.load_more_if_needed(LoadTrigger::Search { token });
    }

    fn continue_search_if_needed(&mut self) {
        let Some(token) = self.search_state.active_token() else {
            return;
        };
        if !self.filtered_rows.is_empty() {
            self.search_state = SearchState::Idle;
            return;
        }
        if self.pagination.reached_scan_limit || self.pagination.next_cursor.is_none() {
            self.search_state = SearchState::Idle;
            return;
        }
        self.load_more_if_needed(LoadTrigger::Search { token });
    }

    fn continue_search_if_token_matches(&mut self, completed_token: Option<usize>) {
        let Some(active) = self.search_state.active_token() else {
            return;
        };
        if let Some(token) = completed_token
            && token != active
        {
            return;
        }
        self.continue_search_if_needed();
    }

    pub(super) fn ensure_selected_visible(&mut self) {
        if self.filtered_rows.is_empty() {
            self.scroll_top = 0;
            return;
        }
        let capacity = self.view_rows.unwrap_or(self.filtered_rows.len()).max(1);

        if self.selected < self.scroll_top {
            self.scroll_top = self.selected;
        } else {
            let last_visible = self.scroll_top.saturating_add(capacity - 1);
            if self.selected > last_visible {
                self.scroll_top = self.selected.saturating_sub(capacity - 1);
            }
        }

        let max_start = self.filtered_rows.len().saturating_sub(capacity);
        if self.scroll_top > max_start {
            self.scroll_top = max_start;
        }
    }

    pub(super) fn ensure_minimum_rows_for_view(&mut self, minimum_rows: usize) {
        if minimum_rows == 0 {
            return;
        }
        if self.filtered_rows.len() >= minimum_rows {
            return;
        }
        if self.pagination.loading.is_pending() || self.pagination.next_cursor.is_none() {
            return;
        }
        if let Some(token) = self.search_state.active_token() {
            self.load_more_if_needed(LoadTrigger::Search { token });
        } else {
            self.load_more_if_needed(LoadTrigger::Scroll);
        }
    }

    pub(super) fn update_view_rows(&mut self, rows: usize) {
        self.view_rows = if rows == 0 { None } else { Some(rows) };
        self.ensure_selected_visible();
    }

    pub(super) fn maybe_load_more_for_scroll(&mut self) {
        if self.pagination.loading.is_pending() {
            return;
        }
        if self.pagination.next_cursor.is_none() {
            return;
        }
        if self.filtered_rows.is_empty() {
            return;
        }
        let remaining = self.filtered_rows.len().saturating_sub(self.selected + 1);
        if remaining <= LOAD_NEAR_THRESHOLD {
            self.load_more_if_needed(LoadTrigger::Scroll);
        }
    }

    fn load_more_if_needed(&mut self, trigger: LoadTrigger) {
        if self.pagination.loading.is_pending() {
            return;
        }
        let Some(cursor) = self.pagination.next_cursor.clone() else {
            return;
        };
        let request_token = self.allocate_request_token();
        let search_token = match trigger {
            LoadTrigger::Scroll => None,
            LoadTrigger::Search { token } => Some(token),
        };
        self.pagination.loading = LoadingState::Pending(PendingLoad {
            request_token,
            search_token,
        });
        self.request_frame();

        (self.page_loader)(PageLoadRequest {
            cursor: Some(cursor),
            request_token,
            search_token,
            default_provider: self.default_provider.clone(),
            sort_key: self.sort_key,
        });
    }

    fn allocate_request_token(&mut self) -> usize {
        let token = self.next_request_token;
        self.next_request_token = self.next_request_token.wrapping_add(1);
        token
    }

    fn allocate_search_token(&mut self) -> usize {
        let token = self.next_search_token;
        self.next_search_token = self.next_search_token.wrapping_add(1);
        token
    }

    /// Cycles the sort order between creation time and last-updated time.
    ///
    /// Triggers a full reload because the backend must re-sort all sessions.
    /// The existing `all_rows` are cleared and pagination restarts from the
    /// beginning with the new sort key.
    pub(super) fn toggle_sort_key(&mut self) {
        self.sort_key = match self.sort_key {
            ProcessSortKey::CreatedAt => ProcessSortKey::UpdatedAt,
            ProcessSortKey::UpdatedAt => ProcessSortKey::CreatedAt,
        };
        self.start_initial_load();
    }
}
