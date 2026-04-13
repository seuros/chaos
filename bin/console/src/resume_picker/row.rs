use std::path::Path;
use std::path::PathBuf;

use chaos_ipc::ProcessId;
use chaos_kern::ProcessItem;
use chaos_kern::path_utils;
use jiff::Timestamp;

#[derive(Clone)]
pub(super) struct Row {
    pub(super) preview: String,
    pub(super) process_id: ProcessId,
    pub(super) process_name: Option<String>,
    pub(super) created_at: Option<Timestamp>,
    pub(super) updated_at: Option<Timestamp>,
    pub(super) cwd: Option<PathBuf>,
    pub(super) git_branch: Option<String>,
}

impl Row {
    pub(super) fn display_preview(&self) -> &str {
        self.process_name.as_deref().unwrap_or(&self.preview)
    }

    pub(super) fn matches_query(&self, query: &str) -> bool {
        if self.preview.to_lowercase().contains(query) {
            return true;
        }
        if let Some(process_name) = self.process_name.as_ref()
            && process_name.to_lowercase().contains(query)
        {
            return true;
        }
        false
    }
}

pub(super) fn rows_from_items(items: Vec<ProcessItem>) -> Vec<Row> {
    items.into_iter().map(|item| head_to_row(&item)).collect()
}

pub(super) fn head_to_row(item: &ProcessItem) -> Row {
    let created_at = item.created_at.as_deref().and_then(parse_timestamp_str);
    let updated_at = item
        .updated_at
        .as_deref()
        .and_then(parse_timestamp_str)
        .or(created_at);

    let preview = item
        .first_user_message
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| String::from("(no message yet)"));
    let Some(process_id) = item.process_id else {
        panic!("journal-backed session picker rows must carry process ids");
    };

    Row {
        preview,
        process_id,
        process_name: None,
        created_at,
        updated_at,
        cwd: item.cwd.clone(),
        git_branch: item.git_branch.clone(),
    }
}

pub(super) fn paths_match(a: &Path, b: &Path) -> bool {
    if let (Ok(ca), Ok(cb)) = (
        path_utils::normalize_for_path_comparison(a),
        path_utils::normalize_for_path_comparison(b),
    ) {
        return ca == cb;
    }
    a == b
}

pub(super) fn parse_timestamp_str(ts: &str) -> Option<Timestamp> {
    ts.parse().ok()
}
