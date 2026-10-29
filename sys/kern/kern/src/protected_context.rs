//! Capture protected files off the async executor and render their durable snapshot.

use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::ProtectedContext;
use chaos_ipc::protocol::ProtectedContextFile;
use chaos_ipc::protocol::RolloutItem;

use crate::config::Config;

pub(crate) async fn load(
    config: &Config,
) -> Result<(Option<ProtectedContext>, Vec<String>), tokio::task::JoinError> {
    if config.protected_context_files.is_empty() {
        return Ok((None, Vec::new()));
    }
    let paths = config.protected_context_files.clone();
    let cwd = config.cwd.clone();
    let max_bytes = config.protected_context_max_bytes;
    tokio::task::spawn_blocking(move || read(&paths, &cwd, max_bytes)).await
}

/// The opened handle must be regular; nonblocking open also refuses a FIFO
/// without waiting for a writer. Symlinks to regular files are allowed.
fn open_regular_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "it is not a regular file",
        ));
    }
    Ok(file)
}

/// Read once, in order, sharing the text allowance across all files. Keep an
/// explicit empty snapshot when configured files could not be read.
fn read(
    paths: &[PathBuf],
    cwd: &Path,
    max_bytes: usize,
) -> (Option<ProtectedContext>, Vec<String>) {
    if paths.is_empty() {
        return (None, Vec::new());
    }
    let mut files = Vec::with_capacity(paths.len());
    let mut warnings = Vec::new();
    let mut remaining = max_bytes;
    let mut truncated = false;
    for path in paths {
        let resolved = if path.is_absolute() {
            path.clone()
        } else {
            cwd.join(path)
        };
        let shown = resolved.display();
        if remaining == 0 {
            truncated = true;
            warnings.push(format!(
                "protected context file {shown} was left out: the {max_bytes}-byte cap was reached"
            ));
            continue;
        }
        let mut bytes = Vec::new();
        let read = open_regular_file(&resolved).and_then(|file| {
            file.take((remaining as u64).saturating_add(1))
                .read_to_end(&mut bytes)
        });
        if let Err(err) = read {
            warnings.push(format!(
                "protected context file {shown} was not read: {err}"
            ));
            continue;
        }
        let cut = bytes.len() > remaining;
        if cut {
            bytes.truncate(remaining);
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(err) if cut && err.utf8_error().error_len().is_none() => {
                // valid_up_to is a character boundary in this same byte buffer.
                let valid = err.utf8_error().valid_up_to();
                String::from_utf8_lossy(&err.as_bytes()[..valid]).into_owned()
            }
            Err(_) => {
                warnings.push(format!(
                    "protected context file {shown} was not read: it is not UTF-8 text"
                ));
                continue;
            }
        };
        if cut {
            truncated = true;
            warnings.push(format!(
                "protected context file {shown} is incomplete: \
                 cut to {} bytes by the {max_bytes}-byte cap",
                text.len()
            ));
        }
        remaining = if cut { 0 } else { remaining - text.len() };
        files.push(ProtectedContextFile {
            path: path.display().to_string(),
            text,
        });
    }
    (Some(ProtectedContext { files, truncated }), warnings)
}

pub(crate) fn render(snapshot: &ProtectedContext) -> String {
    let mut out = String::from(
        "<protected_context>\nThe operator marked these files as protected: \
         they are read once when the session starts \
         and restored verbatim after every compaction.\n",
    );
    for file in &snapshot.files {
        out.push_str("<file path=\"");
        out.push_str(
            &file
                .path
                .replace('&', "&amp;")
                .replace('"', "&quot;")
                .replace('<', "&lt;"),
        );
        out.push_str("\">\n");
        out.push_str(&file.text);
        if !file.text.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("</file>\n");
    }
    if snapshot.truncated {
        out.push_str("[protected context truncated at the configured byte cap]\n");
    }
    out.push_str("</protected_context>");
    out
}

/// Forks append a fresh initial context. Remove only the inherited standalone
/// snapshot messages, including any inside replacement-history checkpoints.
/// Other developer instructions and the original file text are left untouched.
pub(crate) fn remove_inherited(items: &mut Vec<RolloutItem>, snapshot: &ProtectedContext) {
    let rendered = render(snapshot);
    let is_snapshot = |item: &ResponseItem| {
        matches!(item,
            ResponseItem::Message { role, content, .. }
                if matches!(role.as_str(), "system" | "developer")
                    && matches!(content.as_slice(),
                        [ContentItem::InputText { text }] if text == &rendered))
    };
    items.retain_mut(|item| match item {
        RolloutItem::ResponseItem(item) => !is_snapshot(item),
        RolloutItem::Compacted(checkpoint) => {
            checkpoint
                .replacement_history
                .retain(|item| !is_snapshot(item));
            true
        }
        _ => true,
    });
}

#[cfg(test)]
mod tests;
