extern crate usage as usage_rs;

use crossbeam_channel::Receiver;
use crossbeam_channel::Sender;
use crossbeam_channel::after;
use crossbeam_channel::select;
use crossbeam_channel::unbounded;
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::collections::HashSet;
use std::num::NonZero;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use tokio::process::Command;

mod cli;

pub use cli::Cli;

/// A single match result returned from the search.
///
/// * `score` – Local fuzzy relevance score (higher is better).
/// * `path`  – Path to the matched file (relative to the search directory).
/// * `indices` – Optional list of character indices that matched the query.
///   These are only filled when the caller of [`run`] sets
///   `options.compute_indices` to `true`. The indices vector follows the
///   unique and sorted in ascending order so that callers can use them directly
///   for highlighting.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FileMatch {
    pub score: u32,
    pub path: PathBuf,
    pub root: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indices: Option<Vec<u32>>, // Sorted & deduplicated when present
}

impl FileMatch {
    pub fn full_path(&self) -> PathBuf {
        self.root.join(&self.path)
    }
}

/// Returns the final path component for a matched path, falling back to the full path.
pub fn file_name_from_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

#[derive(Debug)]
pub struct FileSearchResults {
    pub matches: Vec<FileMatch>,
    pub total_match_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct FileSearchSnapshot {
    pub query: String,
    pub matches: Vec<FileMatch>,
    pub total_match_count: usize,
    pub scanned_file_count: usize,
    pub walk_complete: bool,
}

#[derive(Debug, Clone)]
pub struct FileSearchOptions {
    pub limit: NonZero<usize>,
    pub exclude: Vec<String>,
    pub threads: NonZero<usize>,
    pub compute_indices: bool,
    /// Whether hidden files and directories should be searched.
    pub include_hidden: bool,
    /// Toggle ignore-file processing in the walker.
    ///
    /// When enabled, ignore files below the search root are honored. Parent
    /// ignore files are never scanned because locate treats each explicit
    /// search root as its boundary. When disabled, the walker turns off
    /// `.gitignore`, git-global/exclude rules, and `.ignore`.
    pub respect_gitignore: bool,
}

impl Default for FileSearchOptions {
    fn default() -> Self {
        Self {
            #[expect(clippy::unwrap_used)]
            limit: NonZero::new(20).unwrap(),
            exclude: Vec::new(),
            #[expect(clippy::unwrap_used)]
            threads: NonZero::new(2).unwrap(),
            compute_indices: false,
            include_hidden: true,
            respect_gitignore: true,
        }
    }
}

pub trait SessionReporter: Send + Sync + 'static {
    /// Called when the debounced top-N changes.
    fn on_update(&self, snapshot: &FileSearchSnapshot);

    /// Called when the session becomes idle or is cancelled. Guaranteed to be called at least once per update_query.
    fn on_complete(&self);
}

pub struct FileSearchSession {
    inner: Arc<SessionInner>,
}

impl FileSearchSession {
    /// Update the query. This should be cheap relative to re-walking.
    pub fn update_query(&self, pattern_text: &str) {
        let _ = self
            .inner
            .work_tx
            .send(WorkSignal::QueryUpdated(pattern_text.to_string()));
    }
}

impl Drop for FileSearchSession {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::Relaxed);
        let _ = self.inner.work_tx.send(WorkSignal::Shutdown);
    }
}

pub fn create_session(
    search_directories: Vec<PathBuf>,
    options: FileSearchOptions,
    reporter: Arc<dyn SessionReporter>,
    cancel_flag: Option<Arc<AtomicBool>>,
) -> anyhow::Result<FileSearchSession> {
    let FileSearchOptions {
        limit,
        exclude,
        threads,
        compute_indices,
        include_hidden,
        respect_gitignore,
    } = options;

    if search_directories.is_empty() {
        anyhow::bail!("at least one search directory is required");
    };
    let (work_tx, work_rx) = unbounded();
    let cancelled = cancel_flag.unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut seen_roots = HashSet::new();

    // Validate every root and exclusion before starting any background work.
    let roots: Vec<_> = search_directories
        .iter()
        .filter(|root| seen_roots.insert((*root).clone()))
        .map(|search_directory| {
            if !std::fs::metadata(search_directory)?.is_dir() {
                anyhow::bail!(
                    "search root must be a directory: {}",
                    search_directory.display()
                );
            }
            anyhow::Ok(RootIndex {
                root: search_directory.clone(),
                override_matcher: build_override_matcher(search_directory, &exclude)?,
                files: Arc::new(RwLock::new(Vec::new())),
                walk_complete: Arc::new(AtomicBool::new(false)),
            })
        })
        .collect::<anyhow::Result<_>>()?;

    for root in &roots {
        spawn_locate_walker(LocateWalkerConfig {
            search_directory: root.root.clone(),
            threads: threads.get(),
            override_matcher: root.override_matcher.clone(),
            include_hidden,
            respect_gitignore,
            files: root.files.clone(),
            walk_complete: root.walk_complete.clone(),
            cancelled: cancelled.clone(),
            shutdown: shutdown.clone(),
        });
    }

    let inner = Arc::new(SessionInner {
        roots,
        limit: limit.get(),
        compute_indices,
        cancelled: cancelled.clone(),
        shutdown,
        reporter,
        work_tx: work_tx.clone(),
    });

    let matcher_inner = inner.clone();
    thread::spawn(move || matcher_worker(matcher_inner, work_rx));

    Ok(FileSearchSession { inner })
}

pub trait Reporter {
    fn report_match(&self, file_match: &FileMatch);
    fn warn_matches_truncated(&self, total_match_count: usize, shown_match_count: usize);
    fn warn_no_search_pattern(&self, search_directory: &Path);
}

pub async fn run_main<T: Reporter>(
    Cli {
        pattern,
        limit,
        cwd,
        compute_indices,
        json: _,
        exclude,
        threads,
    }: Cli,
    reporter: T,
) -> anyhow::Result<()> {
    let search_directory = match cwd {
        Some(dir) => dir,
        None => std::env::current_dir()?,
    };
    let pattern_text = match pattern {
        Some(pattern) => pattern,
        None => {
            reporter.warn_no_search_pattern(&search_directory);
            Command::new("ls")
                .arg("-al")
                .current_dir(search_directory)
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .status()
                .await?;
            return Ok(());
        }
    };

    let FileSearchResults {
        total_match_count,
        matches,
    } = run(
        &pattern_text,
        vec![search_directory.to_path_buf()],
        FileSearchOptions {
            limit,
            exclude,
            threads,
            compute_indices,
            include_hidden: true,
            respect_gitignore: true,
        },
        /*cancel_flag*/ None,
    )?;
    let match_count = matches.len();
    let matches_truncated = total_match_count > match_count;

    for file_match in matches {
        reporter.report_match(&file_match);
    }
    if matches_truncated {
        reporter.warn_matches_truncated(total_match_count, match_count);
    }

    Ok(())
}

/// The worker threads will periodically check `cancel_flag` to see if they
/// should stop processing files.
pub fn run(
    pattern_text: &str,
    roots: Vec<PathBuf>,
    options: FileSearchOptions,
    cancel_flag: Option<Arc<AtomicBool>>,
) -> anyhow::Result<FileSearchResults> {
    let reporter = Arc::new(RunReporter::default());
    let session = create_session(roots, options, reporter.clone(), cancel_flag)?;

    session.update_query(pattern_text);

    let snapshot = reporter.wait_for_complete();
    Ok(FileSearchResults {
        matches: snapshot.matches,
        total_match_count: snapshot.total_match_count,
    })
}

/// Sort matches in-place by descending score, then ascending path.
#[cfg(test)]
fn sort_matches(matches: &mut [(u32, String)]) {
    matches.sort_by(cmp_by_score_desc_then_path_asc::<(u32, String), _, _>(
        |t| t.0,
        |t| t.1.as_str(),
    ));
}

/// Returns a comparator closure suitable for `slice.sort_by(...)` that orders
/// items by descending score and then ascending path using the provided accessors.
pub fn cmp_by_score_desc_then_path_asc<T, FScore, FPath>(
    score_of: FScore,
    path_of: FPath,
) -> impl FnMut(&T, &T) -> std::cmp::Ordering
where
    FScore: Fn(&T) -> u32,
    FPath: Fn(&T) -> &str,
{
    use std::cmp::Ordering;
    move |a, b| match score_of(b).cmp(&score_of(a)) {
        Ordering::Equal => path_of(a).cmp(path_of(b)),
        other => other,
    }
}

struct SessionInner {
    roots: Vec<RootIndex>,
    limit: usize,
    compute_indices: bool,
    cancelled: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    reporter: Arc<dyn SessionReporter>,
    work_tx: Sender<WorkSignal>,
}

struct RootIndex {
    root: PathBuf,
    override_matcher: Option<ignore::overrides::Override>,
    files: Arc<RwLock<Vec<String>>>,
    walk_complete: Arc<AtomicBool>,
}

enum WorkSignal {
    QueryUpdated(String),
    Shutdown,
}

fn build_override_matcher(
    search_directory: &Path,
    exclude: &[String],
) -> anyhow::Result<Option<ignore::overrides::Override>> {
    if exclude.is_empty() {
        return Ok(None);
    }
    let mut override_builder = OverrideBuilder::new(search_directory);
    for exclude in exclude {
        let exclude_pattern = format!("!{exclude}");
        override_builder.add(&exclude_pattern)?;
    }
    let matcher = override_builder.build()?;
    Ok(Some(matcher))
}

struct LocateWalkerConfig {
    search_directory: PathBuf,
    threads: usize,
    override_matcher: Option<ignore::overrides::Override>,
    include_hidden: bool,
    respect_gitignore: bool,
    files: Arc<RwLock<Vec<String>>>,
    walk_complete: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}

fn spawn_locate_walker(walker: LocateWalkerConfig) {
    let LocateWalkerConfig {
        search_directory,
        threads,
        override_matcher,
        include_hidden,
        respect_gitignore,
        files,
        walk_complete,
        cancelled,
        shutdown,
    } = walker;

    thread::spawn(move || {
        let mut walk_builder = WalkBuilder::new(&search_directory);
        walk_builder
            .threads(threads)
            // Hidden files are valid `@` search targets.
            .hidden(!include_hidden)
            .follow_links(true)
            // The explicit root is the boundary, even outside a Git repository.
            // Filter during traversal, including nested ignore files, rather
            // than walking ignored subtrees and discarding matches afterward.
            .git_ignore(respect_gitignore)
            .git_global(respect_gitignore)
            .git_exclude(respect_gitignore)
            .ignore(respect_gitignore)
            .require_git(false)
            .parents(false)
            .filter_entry(|entry| entry.file_name() != ".git");
        if let Some(override_matcher) = override_matcher {
            walk_builder.overrides(override_matcher);
        }

        let walker = walk_builder.build_parallel();
        walker.run(|| {
            let search_directory = search_directory.clone();
            let files = files.clone();
            let cancelled = cancelled.clone();
            let shutdown = shutdown.clone();

            Box::new(move |entry| {
                if cancelled.load(Ordering::Relaxed) || shutdown.load(Ordering::Relaxed) {
                    return ignore::WalkState::Quit;
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => return ignore::WalkState::Continue,
                };
                if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                    return ignore::WalkState::Continue;
                }
                let relative_path = entry
                    .path()
                    .strip_prefix(&search_directory)
                    .ok()
                    .and_then(Path::to_str);
                if let (Some(relative_path), Ok(mut guard)) = (relative_path, files.write()) {
                    guard.push(relative_path.to_string());
                }
                ignore::WalkState::Continue
            })
        });
        walk_complete.store(true, Ordering::Release);
    });
}

fn matcher_worker(inner: Arc<SessionInner>, work_rx: Receiver<WorkSignal>) -> anyhow::Result<()> {
    const POLL_INTERVAL_MS: u64 = 10;
    let cancel_requested = || inner.cancelled.load(Ordering::Relaxed);
    let shutdown_requested = || inner.shutdown.load(Ordering::Relaxed);

    let mut last_query = None;
    let mut needs_search = false;
    let mut completed_for_query = false;

    loop {
        select! {
            recv(work_rx) -> signal => {
                let Ok(signal) = signal else {
                    break;
                };
                match signal {
                    WorkSignal::QueryUpdated(query) => {
                        last_query = Some(query);
                        needs_search = true;
                        completed_for_query = false;
                        if !scan_complete(&inner) {
                            let snapshot = search_index(&inner, last_query.as_deref().unwrap_or(""), false);
                            inner.reporter.on_update(&snapshot);
                        }
                    }
                    WorkSignal::Shutdown => {
                        break;
                    }
                }
            }
            recv(after(Duration::from_millis(POLL_INTERVAL_MS))) -> _ => {
                let Some(query) = last_query.as_deref() else {
                    continue;
                };
                let walk_complete = scan_complete(&inner);
                if needs_search || !walk_complete || !completed_for_query {
                    let snapshot = search_index(&inner, query, walk_complete);
                    inner.reporter.on_update(&snapshot);
                    needs_search = false;
                }
                if walk_complete && !completed_for_query {
                    inner.reporter.on_complete();
                    completed_for_query = true;
                }
            }
        }

        if cancel_requested() || shutdown_requested() {
            break;
        }
    }

    // If we cancelled or otherwise exited the loop, make sure the reporter is notified.
    inner.reporter.on_complete();

    Ok(())
}

fn scan_complete(inner: &SessionInner) -> bool {
    inner
        .roots
        .iter()
        .all(|root| root.walk_complete.load(Ordering::Acquire))
}

fn search_index(inner: &SessionInner, query_text: &str, walk_complete: bool) -> FileSearchSnapshot {
    if inner.cancelled.load(Ordering::Relaxed) {
        return FileSearchSnapshot {
            query: query_text.to_string(),
            matches: Vec::new(),
            total_match_count: 0,
            scanned_file_count: 0,
            walk_complete,
        };
    }

    // The heap retains only top-N paths. Its greatest element is the worst
    // candidate: lowest score, then lexicographically greatest path/root.
    let mut matches = BinaryHeap::new();
    let mut total_match_count = 0usize;
    let mut scanned_file_count = 0usize;

    for root in &inner.roots {
        let Ok(files) = root.files.read() else {
            continue;
        };
        scanned_file_count = scanned_file_count.saturating_add(files.len());
        for relative_path in files.iter() {
            if inner.cancelled.load(Ordering::Relaxed) || inner.shutdown.load(Ordering::Relaxed) {
                return FileSearchSnapshot {
                    query: query_text.to_string(),
                    walk_complete,
                    ..Default::default()
                };
            }
            let Some(score) = fuzzy_subsequence_score(query_text, relative_path) else {
                continue;
            };
            total_match_count = total_match_count.saturating_add(1);
            let candidate = (
                Reverse(score),
                PathBuf::from(relative_path),
                root.root.clone(),
            );
            if matches.len() < inner.limit {
                matches.push(candidate);
            } else if matches.peek().is_some_and(|worst| candidate < *worst) {
                matches.pop();
                matches.push(candidate);
            }
        }
    }

    let matches = matches
        .into_sorted_vec()
        .into_iter()
        .map(|(Reverse(score), path, root)| FileMatch {
            score,
            indices: inner
                .compute_indices
                .then(|| fuzzy_subsequence_indices(query_text, path.to_str().unwrap_or(""))),
            path,
            root,
        })
        .collect();

    FileSearchSnapshot {
        query: query_text.to_string(),
        matches,
        total_match_count,
        scanned_file_count,
        walk_complete,
    }
}

pub fn fuzzy_subsequence_score(query: &str, haystack: &str) -> Option<u32> {
    let indices = fuzzy_subsequence_indices(query, haystack);
    let query_len = query.chars().filter(|ch| !ch.is_whitespace()).count();
    if query_len == 0 || indices.len() != query_len {
        return None;
    }
    let span = indices
        .last()
        .zip(indices.first())
        .map(|(last, first)| last.saturating_sub(*first).saturating_add(1))
        .unwrap_or(0);
    let normalized_query = normalize_for_path_match(query);
    let normalized_haystack = normalize_for_path_match(haystack);
    let normalized_basename = normalize_for_path_match(&file_name_from_path(haystack));
    let query_lower = query.to_lowercase();
    let haystack_lower = haystack.to_lowercase();

    let mut score = 10_000u32
        .saturating_sub(span)
        .saturating_sub(haystack.len() as u32);
    if haystack_lower.contains(&query_lower) {
        score = score.saturating_add(50_000);
    }
    if !normalized_query.is_empty() && normalized_haystack.contains(&normalized_query) {
        score = score.saturating_add(40_000);
    }
    if !normalized_query.is_empty() && normalized_basename.contains(&normalized_query) {
        score = score.saturating_add(20_000);
    }
    Some(score)
}

fn fuzzy_subsequence_indices(query: &str, haystack: &str) -> Vec<u32> {
    let mut indices = Vec::new();
    let mut query_chars = query
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .flat_map(char::to_lowercase);
    let Some(mut needle) = query_chars.next() else {
        return indices;
    };
    for (idx, ch) in haystack.chars().enumerate() {
        if ch.to_lowercase().any(|lower| lower == needle) {
            indices.push(idx as u32);
            let Some(next) = query_chars.next() else {
                break;
            };
            needle = next;
        }
    }
    indices
}

fn normalize_for_path_match(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Default)]
struct RunReporter {
    snapshot: RwLock<FileSearchSnapshot>,
    completed: (Condvar, Mutex<bool>),
}

impl SessionReporter for RunReporter {
    fn on_update(&self, snapshot: &FileSearchSnapshot) {
        #[expect(clippy::unwrap_used)]
        let mut guard = self.snapshot.write().unwrap();
        *guard = snapshot.clone();
    }

    fn on_complete(&self) {
        let (cv, mutex) = &self.completed;
        let mut completed = mutex.lock().unwrap();
        *completed = true;
        cv.notify_all();
    }
}

impl RunReporter {
    fn wait_for_complete(&self) -> FileSearchSnapshot {
        let (cv, mutex) = &self.completed;
        let mut completed = mutex.lock().unwrap();
        while !*completed {
            completed = cv.wait(completed).unwrap();
        }
        self.snapshot.read().unwrap().clone()
    }
}

#[cfg(test)]
mod tests;
