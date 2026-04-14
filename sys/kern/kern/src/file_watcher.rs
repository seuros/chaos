//! Watches config paths for changes and broadcasts coarse-grained
//! `FileWatcherEvent`s that higher-level components react to on the next turn.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::time::Duration;

use notify::Event;
use notify::EventKind;
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher;
use tokio::runtime::Handle;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::sleep_until;
use tracing::warn;

use crate::config::Config;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileWatcherEvent {
    ConfigChanged { paths: Vec<PathBuf> },
}

struct WatchState {
    roots_ref_counts: HashMap<PathBuf, usize>,
}

struct FileWatcherInner {
    watcher: RecommendedWatcher,
    watched_paths: HashMap<PathBuf, RecursiveMode>,
}

const WATCHER_THROTTLE_INTERVAL: Duration = Duration::from_secs(10);

/// Coalesces bursts of paths and emits at most once per interval.
struct ThrottledPaths {
    pending: HashSet<PathBuf>,
    next_allowed_at: Instant,
}

impl ThrottledPaths {
    fn new(now: Instant) -> Self {
        Self {
            pending: HashSet::new(),
            next_allowed_at: now,
        }
    }

    fn add(&mut self, paths: Vec<PathBuf>) {
        self.pending.extend(paths);
    }

    fn next_deadline(&self, now: Instant) -> Option<Instant> {
        (!self.pending.is_empty() && now < self.next_allowed_at).then_some(self.next_allowed_at)
    }

    fn take_ready(&mut self, now: Instant) -> Option<Vec<PathBuf>> {
        if self.pending.is_empty() || now < self.next_allowed_at {
            return None;
        }
        Some(self.take_with_next_allowed(now))
    }

    fn take_pending(&mut self, now: Instant) -> Option<Vec<PathBuf>> {
        if self.pending.is_empty() {
            return None;
        }
        Some(self.take_with_next_allowed(now))
    }

    fn take_with_next_allowed(&mut self, now: Instant) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self.pending.drain().collect();
        paths.sort_unstable_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
        self.next_allowed_at = now + WATCHER_THROTTLE_INTERVAL;
        paths
    }
}

pub(crate) struct FileWatcher {
    inner: Option<Mutex<FileWatcherInner>>,
    state: Arc<RwLock<WatchState>>,
    tx: broadcast::Sender<FileWatcherEvent>,
}

pub(crate) struct WatchRegistration {
    file_watcher: std::sync::Weak<FileWatcher>,
    roots: Vec<PathBuf>,
}

impl Drop for WatchRegistration {
    fn drop(&mut self) {
        if let Some(file_watcher) = self.file_watcher.upgrade() {
            file_watcher.unregister_roots(&self.roots);
        }
    }
}

impl FileWatcher {
    pub(crate) fn new(_codex_home: PathBuf) -> notify::Result<Self> {
        let (raw_tx, raw_rx) = mpsc::unbounded_channel();
        let raw_tx_clone = raw_tx;
        let watcher = notify::recommended_watcher(move |res| {
            let _ = raw_tx_clone.send(res);
        })?;
        let inner = FileWatcherInner {
            watcher,
            watched_paths: HashMap::new(),
        };
        let (tx, _) = broadcast::channel(128);
        let state = Arc::new(RwLock::new(WatchState {
            roots_ref_counts: HashMap::new(),
        }));
        let file_watcher = Self {
            inner: Some(Mutex::new(inner)),
            state: Arc::clone(&state),
            tx: tx.clone(),
        };
        file_watcher.spawn_event_loop(raw_rx, state, tx);
        Ok(file_watcher)
    }

    pub(crate) fn noop() -> Self {
        let (tx, _) = broadcast::channel(1);
        Self {
            inner: None,
            state: Arc::new(RwLock::new(WatchState {
                roots_ref_counts: HashMap::new(),
            })),
            tx,
        }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<FileWatcherEvent> {
        self.tx.subscribe()
    }

    pub(crate) fn register_config(self: &Arc<Self>, _config: &Config) -> WatchRegistration {
        WatchRegistration {
            file_watcher: Arc::downgrade(self),
            roots: Vec::new(),
        }
    }

    // Bridge `notify`'s callback-based events into the Tokio runtime and
    // broadcast coarse-grained change signals to subscribers.
    fn spawn_event_loop(
        &self,
        mut raw_rx: mpsc::UnboundedReceiver<notify::Result<Event>>,
        state: Arc<RwLock<WatchState>>,
        tx: broadcast::Sender<FileWatcherEvent>,
    ) {
        if let Ok(handle) = Handle::try_current() {
            handle.spawn(async move {
                let now = Instant::now();
                let mut pending = ThrottledPaths::new(now);

                loop {
                    let now = Instant::now();
                    let next_deadline = pending.next_deadline(now);
                    let timer_deadline = next_deadline
                        .unwrap_or_else(|| now + Duration::from_secs(60 * 60 * 24 * 365));
                    let timer = sleep_until(timer_deadline);
                    tokio::pin!(timer);

                    tokio::select! {
                        res = raw_rx.recv() => {
                            match res {
                                Some(Ok(event)) => {
                                    let paths = classify_event(&event, &state);
                                    let now = Instant::now();
                                    pending.add(paths);

                                    if let Some(paths) = pending.take_ready(now) {
                                        let _ = tx.send(FileWatcherEvent::ConfigChanged { paths });
                                    }
                                }
                                Some(Err(err)) => {
                                    warn!("file watcher error: {err}");
                                }
                                None => {
                                    // Flush any pending changes before shutdown so subscribers
                                    // see the latest state.
                                    let now = Instant::now();
                                    if let Some(paths) = pending.take_pending(now) {
                                        let _ = tx.send(FileWatcherEvent::ConfigChanged { paths });
                                    }
                                    break;
                                }
                            }
                        }
                        _ = &mut timer => {
                            let now = Instant::now();
                            if let Some(paths) = pending.take_ready(now) {
                                let _ = tx.send(FileWatcherEvent::ConfigChanged { paths });
                            }
                        }
                    }
                }
            });
        } else {
            warn!("file watcher loop skipped: no Tokio runtime available");
        }
    }

    #[allow(dead_code)]
    pub(crate) fn register_root(&self, root: PathBuf) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = state.roots_ref_counts.entry(root.clone()).or_insert(0);
        *count += 1;
        if *count == 1 {
            self.watch_path(root, RecursiveMode::Recursive);
        }
    }

    fn unregister_roots(&self, roots: &[PathBuf]) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut inner_guard: Option<std::sync::MutexGuard<'_, FileWatcherInner>> = None;

        for root in roots {
            let mut should_unwatch = false;
            if let Some(count) = state.roots_ref_counts.get_mut(root) {
                if *count > 1 {
                    *count -= 1;
                } else {
                    state.roots_ref_counts.remove(root);
                    should_unwatch = true;
                }
            }

            if !should_unwatch {
                continue;
            }
            let Some(inner) = &self.inner else {
                continue;
            };
            if inner_guard.is_none() {
                let guard = inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                inner_guard = Some(guard);
            }

            let Some(guard) = inner_guard.as_mut() else {
                continue;
            };
            if guard.watched_paths.remove(root).is_none() {
                continue;
            }
            // Ignore errors — the watch may already be gone if the
            // directory was removed or the OS evicted the inotify entry.
            let _ = guard.watcher.unwatch(root);
        }
    }

    #[allow(dead_code)]
    fn watch_path(&self, path: PathBuf, mode: RecursiveMode) {
        let Some(inner) = &self.inner else {
            return;
        };
        if !path.exists() {
            return;
        }
        let watch_path = path;
        let mut guard = inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = guard.watched_paths.get(&watch_path) {
            if *existing == RecursiveMode::Recursive || *existing == mode {
                return;
            }
            let _ = guard.watcher.unwatch(&watch_path);
        }
        if let Err(err) = guard.watcher.watch(&watch_path, mode) {
            warn!("failed to watch {}: {err}", watch_path.display());
            return;
        }
        guard.watched_paths.insert(watch_path, mode);
    }
}

fn classify_event(event: &Event, state: &RwLock<WatchState>) -> Vec<PathBuf> {
    if !matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) {
        return Vec::new();
    }

    let roots = match state.read() {
        Ok(state) => state
            .roots_ref_counts
            .keys()
            .cloned()
            .collect::<HashSet<_>>(),
        Err(err) => {
            let state = err.into_inner();
            state
                .roots_ref_counts
                .keys()
                .cloned()
                .collect::<HashSet<_>>()
        }
    };

    event
        .paths
        .iter()
        .filter(|path| roots.iter().any(|root| path.starts_with(root)))
        .cloned()
        .collect()
}

#[cfg(test)]
#[path = "file_watcher_tests.rs"]
mod tests;
