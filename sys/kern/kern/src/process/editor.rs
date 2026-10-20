use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use super::Process;

/// A bounded editor request. Transport implementations own cancellation and
/// acknowledgement accounting; a failed write must never be retried.
pub type EditorFileFuture<'a, T> = Pin<Box<dyn Future<Output = io::Result<T>> + Send + 'a>>;

/// Optional process-local editor text transport, not a policy authority.
pub trait EditorFileAccess: Send + Sync {
    /// Whether text replacement or creation was negotiated.
    fn can_write(&self) -> bool;
    /// Read current UTF-8 editor text, without saving it. Only a confirmed
    /// absent resource may return `io::ErrorKind::NotFound`.
    fn read<'a>(&'a self, path: &'a Path) -> EditorFileFuture<'a, String>;
    /// Replace editor text. Failure after dispatch means an unknown outcome.
    fn write<'a>(&'a self, path: &'a Path, content: String) -> EditorFileFuture<'a, ()>;
}

struct Binding {
    turn_id: String,
    access: Arc<dyn EditorFileAccess>,
}

#[derive(Default)]
struct State {
    selected: bool,
    binding: Option<Binding>,
}

#[derive(Clone, Default)]
pub(crate) struct EditorFiles(Arc<Mutex<State>>);

impl EditorFiles {
    pub(crate) fn for_turn(&self, turn_id: &str) -> io::Result<Option<Arc<dyn EditorFileAccess>>> {
        let state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match &state.binding {
            Some(binding) if binding.turn_id == turn_id => Ok(Some(binding.access.clone())),
            _ if state.selected => Err(io::Error::other(
                "editor access is unavailable outside its originating foreground turn",
            )),
            _ => Ok(None),
        }
    }
}

/// Drops the foreground binding, but never silently reselects disk state.
pub struct EditorFileScope(EditorFiles);

impl Drop for EditorFileScope {
    fn drop(&mut self) {
        self.0
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .binding = None;
    }
}

impl Process {
    /// Bind editor text to an explicitly identified native submission before
    /// submitting it. Descendants and background continuations do not inherit it.
    pub fn bind_editor_files(
        &self,
        turn_id: String,
        access: Arc<dyn EditorFileAccess>,
    ) -> io::Result<EditorFileScope> {
        let files = &self.chaos.session.services.editor_files;
        let mut state = files.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.binding.is_some() || turn_id.is_empty() {
            return Err(io::Error::other(
                "editor foreground binding is already active or invalid",
            ));
        }
        state.selected = true;
        state.binding = Some(Binding { turn_id, access });
        Ok(EditorFileScope(files.clone()))
    }
}
