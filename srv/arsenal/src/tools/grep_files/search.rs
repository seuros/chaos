//! Shared traversal and bounded pagination for content and files-only searches.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use globset::Glob;
use ignore::WalkBuilder;
use regex::bytes::RegexBuilder;
use serde::Serialize;

use super::GrepFilesParams;
use super::MAX_FILE_SIZE;
use super::MAX_LIMIT;
use super::MAX_OUTPUT_BYTES;
use super::OutputMode;
use super::content;

#[derive(Serialize)]
#[serde(untagged)]
enum SearchEntry {
    File(String),
    Content(content::ContentMatch),
}

#[derive(Default)]
struct SearchPage {
    matches: Vec<SearchEntry>,
    seen: usize,
    output_bytes: usize,
    has_more: bool,
}

impl SearchPage {
    /// False means a result was withheld by a bound; its offset remains resumable.
    fn add(
        &mut self,
        params: &GrepFilesParams,
        entry: impl FnOnce() -> SearchEntry,
    ) -> Result<bool, String> {
        let index = self.seen;
        self.seen = self.seen.saturating_add(1);
        if index < params.offset {
            return Ok(true);
        }
        if self.matches.len() >= params.limit.min(MAX_LIMIT) {
            self.has_more = true;
            return Ok(false);
        }
        let entry = entry();
        let bytes = serde_json::to_vec(&entry)
            .map_err(|e| format!("unable to encode search match: {e}"))?
            .len();
        if self.output_bytes + bytes > MAX_OUTPUT_BYTES {
            self.has_more = true;
            return Ok(false);
        }
        self.output_bytes += bytes;
        self.matches.push(entry);
        Ok(true)
    }
}

pub(super) fn run_search(params: &GrepFilesParams) -> Result<serde_json::Value, String> {
    let case_sensitive = params
        .case_sensitive
        .unwrap_or_else(|| params.pattern.chars().any(char::is_uppercase));
    let regex = RegexBuilder::new(&params.pattern)
        .case_insensitive(!case_sensitive)
        .multi_line(true)
        .unicode(false)
        .build()
        .map_err(|e| format!("invalid regex pattern: {e}"))?;
    let include = params
        .include
        .as_deref()
        .map(str::trim)
        .filter(|val| !val.is_empty());
    let include_matcher = include
        .map(|pattern| {
            Glob::new(pattern)
                .map(|glob| glob.compile_matcher())
                .map_err(|e| format!("invalid include glob: {e}"))
        })
        .transpose()?;
    let search_path = std::path::absolute(params.path.as_deref().unwrap_or("."))
        .map_err(|e| format!("unable to resolve search path: {e}"))?;
    std::fs::metadata(&search_path)
        .map_err(|e| format!("unable to access `{}`: {e}", search_path.display()))?;
    let mut walker = WalkBuilder::new(&search_path);
    walker
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git");

    let mut files = Vec::new();
    for entry in walker.build() {
        // Ignore unreadable/vanished entries without losing other matches.
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        if let Some(matcher) = &include_matcher {
            let match_path = if include.is_some_and(|pattern| pattern.contains('/')) {
                path.strip_prefix(&search_path).unwrap_or(path)
            } else {
                Path::new(entry.file_name())
            };
            if !matcher.is_match(match_path) {
                continue;
            }
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() == 0 || metadata.len() > MAX_FILE_SIZE {
            continue;
        }
        files.push((path.to_path_buf(), metadata.modified().ok()));
    }
    // Newest first, with path breaking ties, in both output modes.
    files.sort_by(|(left_path, left_time), (right_path, right_time)| {
        right_time
            .cmp(left_time)
            .then_with(|| left_path.cmp(right_path))
    });
    let mut page = SearchPage::default();
    let mut contents = Vec::new();
    'files: for (path, _) in files {
        let Ok(file) = File::open(&path) else {
            continue;
        };
        if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
            continue;
        }
        contents.clear();
        if file
            .take(MAX_FILE_SIZE + 1)
            .read_to_end(&mut contents)
            .is_err()
            || contents.len() as u64 > MAX_FILE_SIZE
            || contents[..contents.len().min(8192)].contains(&0)
        {
            continue;
        }
        match params.output_mode {
            OutputMode::Files => {
                if regex.is_match(&contents)
                    && !page.add(params, || {
                        SearchEntry::File(path.to_string_lossy().into_owned())
                    })?
                {
                    break;
                }
            }
            OutputMode::Content => {
                for line in content::matching_lines(&contents, &regex) {
                    if !page.add(params, || {
                        SearchEntry::Content(content::snippet(
                            &path,
                            &contents,
                            line,
                            params.context_lines,
                        ))
                    })? {
                        break 'files;
                    }
                }
            }
        }
    }
    let match_count = page.matches.len();
    Ok(serde_json::json!({
        "output_mode": params.output_mode,
        "matches": page.matches,
        "match_count": match_count,
        "limit": params.limit.min(MAX_LIMIT),
        "offset": params.offset,
        "has_more": page.has_more,
        "next_offset": page.has_more.then(|| params.offset.saturating_add(match_count)),
    }))
}
