mod invocation;
mod parser;
mod seek_sequence;
mod standalone_executable;
mod text;

use std::collections::HashMap;
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::path::PathBuf;

pub use parser::Hunk;
pub use parser::ParseError;
use parser::ParseError::*;
use parser::UpdateFileChunk;
pub use parser::parse_patch;
use similar::TextDiff;
use thiserror::Error;

pub use invocation::maybe_parse_apply_patch_verified;
pub use invocation::{MaybeApplyPatch, maybe_parse_apply_patch};
pub use standalone_executable::main;
pub use text::{prepare_text_patch, prepare_text_update, text_patch_path, text_update_path};

use crate::invocation::ExtractHeredocError;

/// Detailed instructions for gpt-4.1 on how to use the `apply_patch` tool.
pub const APPLY_PATCH_TOOL_INSTRUCTIONS: &str = include_str!("../apply_patch_tool_instructions.md");

/// Special argv[1] flag used when the Chaos executable self-invokes to run the
/// internal `apply_patch` path.
///
/// Although this constant lives in `chaos-apply-patch` (to avoid forcing
/// `chaos-arg0` to depend on `chaos-kern`), it is part of the "Chaos core"
/// process-invocation contract between the apply-patch runtime and the arg0
/// dispatcher.
pub const CHAOS_CORE_APPLY_PATCH_ARG1: &str = "--chaos-run-as-apply-patch";

#[derive(Debug, Error, PartialEq)]
pub enum ApplyPatchError {
    #[error(transparent)]
    ParseError(#[from] ParseError),
    #[error(transparent)]
    IoError(#[from] IoError),
    /// Error that occurs while computing replacements when applying patch chunks
    #[error("{0}")]
    ComputeReplacements(String),
    /// A raw patch body was provided without an explicit `apply_patch` invocation.
    #[error(
        "patch detected without explicit call to apply_patch. \
         Rerun as [\"apply_patch\", \"<patch>\"]"
    )]
    ImplicitInvocation,
}

impl From<std::io::Error> for ApplyPatchError {
    fn from(err: std::io::Error) -> Self {
        ApplyPatchError::IoError(IoError {
            context: "I/O error".to_string(),
            source: err,
        })
    }
}

impl From<&std::io::Error> for ApplyPatchError {
    fn from(err: &std::io::Error) -> Self {
        ApplyPatchError::IoError(IoError {
            context: "I/O error".to_string(),
            source: std::io::Error::new(err.kind(), err.to_string()),
        })
    }
}

#[derive(Debug, Error)]
#[error("{context}: {source}")]
pub struct IoError {
    context: String,
    #[source]
    source: std::io::Error,
}

impl PartialEq for IoError {
    fn eq(&self, other: &Self) -> bool {
        self.context == other.context && self.source.to_string() == other.source.to_string()
    }
}

/// Both the raw PATCH argument to `apply_patch` as well as the PATCH argument
/// parsed into hunks.
#[derive(Debug, PartialEq)]
pub struct ApplyPatchArgs {
    pub patch: String,
    pub hunks: Vec<Hunk>,
    pub workdir: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum ApplyPatchFileChange {
    Add {
        content: String,
    },
    Delete {
        content: String,
    },
    Update {
        unified_diff: String,
        move_path: Option<PathBuf>,
        /// new_content that will result after the unified_diff is applied.
        new_content: String,
        /// Original snapshot required for an implicit update from Add File.
        expected_original: Option<String>,
    },
}

#[derive(Debug, PartialEq)]
pub enum MaybeApplyPatchVerified {
    /// `argv` corresponded to an `apply_patch` invocation, and these are the
    /// resulting proposed file changes.
    Body(ApplyPatchAction),
    /// `argv` could not be parsed to determine whether it corresponds to an
    /// `apply_patch` invocation.
    ShellParseError(ExtractHeredocError),
    /// `argv` corresponded to an `apply_patch` invocation, but it could not
    /// be fulfilled due to the specified error.
    CorrectnessError(ApplyPatchError),
    /// `argv` decidedly did not correspond to an `apply_patch` invocation.
    NotApplyPatch,
}

/// ApplyPatchAction is the result of parsing an `apply_patch` command. By
/// construction, all paths should be absolute paths.
#[derive(Debug, PartialEq)]
pub struct ApplyPatchAction {
    /// The raw patch argument that can be used with `apply_patch` as an exec
    /// call. i.e., if the original arg was parsed in "lenient" mode with a
    /// heredoc, this should be the value without the heredoc wrapper.
    pub patch: String,

    /// The working directory that was used to resolve relative paths in the patch.
    pub cwd: PathBuf,
    changes: HashMap<PathBuf, ApplyPatchFileChange>,
}

impl ApplyPatchAction {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Returns the changes that would be made by applying the patch.
    pub fn changes(&self) -> &HashMap<PathBuf, ApplyPatchFileChange> {
        &self.changes
    }

    /// Reconstruct an `ApplyPatchAction` from its constituent parts.
    ///
    /// Used by the fork-based apply_patch runtime on Linux to rebuild the
    /// action in the child process after deserializing across the fork boundary.
    pub fn from_parts(
        changes: HashMap<PathBuf, ApplyPatchFileChange>,
        cwd: PathBuf,
        patch: String,
    ) -> Self {
        Self {
            changes,
            cwd,
            patch,
        }
    }

    /// Should be used exclusively for testing. (Not worth the overhead of
    /// creating a feature flag for this.)
    pub fn new_add_for_test(path: &Path, content: String) -> Self {
        if !path.is_absolute() {
            panic!("path must be absolute");
        }

        #[expect(clippy::expect_used)]
        let filename = path
            .file_name()
            .expect("path should not be empty")
            .to_string_lossy();
        let patch = format!(
            r#"*** Begin Patch
*** Update File: {filename}
@@
+ {content}
*** End Patch"#,
        );
        let changes = HashMap::from([(path.to_path_buf(), ApplyPatchFileChange::Add { content })]);
        #[expect(clippy::expect_used)]
        Self {
            changes,
            cwd: path
                .parent()
                .expect("path should have parent")
                .to_path_buf(),
            patch,
        }
    }
}

/// Applies the patch and prints the result to stdout/stderr.
pub fn apply_patch(
    patch: &str,
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
) -> Result<(), ApplyPatchError> {
    let hunks = match parse_patch(patch) {
        Ok(source) => source.hunks,
        Err(e) => {
            match &e {
                InvalidPatchError(message) => {
                    writeln!(stderr, "Invalid patch: {message}").map_err(ApplyPatchError::from)?;
                }
                InvalidHunkError {
                    message,
                    line_number,
                } => {
                    writeln!(
                        stderr,
                        "Invalid patch hunk on line {line_number}: {message}"
                    )
                    .map_err(ApplyPatchError::from)?;
                }
            }
            return Err(ApplyPatchError::ParseError(e));
        }
    };

    apply_hunks(&hunks, stdout, stderr)?;

    Ok(())
}

/// Applies hunks and continues to update stdout/stderr
pub fn apply_hunks(
    hunks: &[Hunk],
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
) -> Result<(), ApplyPatchError> {
    match apply_hunks_to_files(hunks) {
        Ok(affected) => Ok(print_summary(&affected, stdout)?),
        Err(err) => {
            writeln!(stderr, "{err}")?;
            Err(err)
        }
    }
}

/// Tracks file paths affected by applying a patch.
#[derive(Default)]
pub struct AffectedPaths {
    pub added: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
}

/// Apply the hunks to the filesystem, returning which files were added, modified, or deleted.
/// Returns an error if the patch could not be applied.
fn apply_hunks_to_files(hunks: &[Hunk]) -> Result<AffectedPaths, ApplyPatchError> {
    if hunks.is_empty() {
        return Err(empty_patch());
    }

    // Prepare each hunk immediately before applying it: the CLI applies hunks
    // in order, so a later hunk can see changes from an earlier one.
    let mut affected = AffectedPaths::default();
    for hunk in hunks {
        match hunk {
            Hunk::AddFile { path, contents } => {
                affected.apply_change(path, &prepare_add(path, contents.clone())?)?;
            }
            Hunk::DeleteFile { path } => affected.delete(path)?,
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let patch = derive_new_contents_from_chunks(path, chunks)?;
                affected.update(path, move_path.as_deref(), &patch.new_contents, None)?;
            }
        }
    }
    Ok(affected)
}

fn empty_patch() -> ApplyPatchError {
    ApplyPatchError::ComputeReplacements("No files were modified.".into())
}

impl AffectedPaths {
    fn apply_change(&mut self, path: &Path, change: &ApplyPatchFileChange) -> Result<(), IoError> {
        match change {
            ApplyPatchFileChange::Add { content } => {
                create_file(path, content)?;
                self.added.push(path.to_path_buf());
                Ok(())
            }
            ApplyPatchFileChange::Delete { .. } => self.delete(path),
            ApplyPatchFileChange::Update {
                new_content,
                move_path,
                expected_original,
                ..
            } => self.update(
                path,
                move_path.as_deref(),
                new_content,
                expected_original.as_deref(),
            ),
        }
    }

    fn delete(&mut self, path: &Path) -> Result<(), IoError> {
        remove_file(path, "delete file")?;
        self.deleted.push(path.to_path_buf());
        Ok(())
    }

    fn update(
        &mut self,
        path: &Path,
        move_path: Option<&Path>,
        content: &str,
        expected_original: Option<&str>,
    ) -> Result<(), IoError> {
        if let Some(original) = expected_original {
            if apply_small_update(path, original, content)? {
                self.modified.push(path.to_path_buf());
            }
            return Ok(());
        }

        let dest = move_path.unwrap_or(path);
        create_parent_dirs(dest)?;
        std::fs::write(dest, content).map_err(|source| IoError {
            context: format!("Failed to write file {}", dest.display()),
            source,
        })?;
        if move_path.is_some() {
            remove_file(path, "remove original")?;
        }
        self.modified.push(dest.to_path_buf());
        Ok(())
    }
}

fn remove_file(path: &Path, operation: &str) -> Result<(), IoError> {
    std::fs::remove_file(path).map_err(|source| IoError {
        context: format!("Failed to {operation} {}", path.display()),
        source,
    })
}

struct AppliedPatch {
    original_contents: String,
    new_contents: String,
}

fn add_file_error(path: &Path, source: std::io::Error) -> IoError {
    let context = if source.kind() == std::io::ErrorKind::AlreadyExists {
        format!(
            "Cannot add {}: use Update File for an existing path",
            path.display()
        )
    } else {
        format!("Failed to create file {}", path.display())
    };
    IoError { context, source }
}

const SMALL_ADD_MAX_CHANGED_LINES: usize = 8;
const SMALL_ADD_MAX_CHANGED_BYTES: usize = 4096;

fn prepare_add(path: &Path, content: String) -> Result<ApplyPatchFileChange, ApplyPatchError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ApplyPatchFileChange::Add { content });
        }
        Err(error) => return Err(add_file_error(path, error).into()),
    };
    if !metadata.is_file() {
        return Err(add_file_error(path, std::io::ErrorKind::AlreadyExists.into()).into());
    }
    let original = std::fs::read_to_string(path).map_err(|source| IoError {
        context: format!("Failed to read existing file {}", path.display()),
        source,
    })?;
    let diff = TextDiff::from_lines(&original, &content);
    let (mut added, mut removed, mut bytes) = (0, 0, 0);
    let (mut removed_text, mut retained_text) = (false, false);
    for change in diff.iter_all_changes() {
        let line = change.value();
        match change.tag() {
            similar::ChangeTag::Equal => {
                retained_text |= !line.trim().is_empty();
                continue;
            }
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Delete => {
                removed += 1;
                removed_text |= !line.trim().is_empty();
            }
        }
        bytes += line.len();
    }
    if added + removed > SMALL_ADD_MAX_CHANGED_LINES
        || bytes > SMALL_ADD_MAX_CHANGED_BYTES
        || (removed_text && !retained_text)
    {
        return Err(ApplyPatchError::ComputeReplacements(format!(
            "Cannot add {}: would add {added} and remove {removed} lines; use Update File",
            path.display()
        )));
    }
    Ok(ApplyPatchFileChange::Update {
        unified_diff: diff.unified_diff().context_radius(1).to_string(),
        move_path: None,
        new_content: content,
        expected_original: Some(original),
    })
}

fn apply_small_update(path: &Path, original: &str, content: &str) -> Result<bool, IoError> {
    let error = |source| IoError {
        context: format!("Failed to apply small update to {}", path.display()),
        source,
    };
    let changed = || {
        error(std::io::Error::other(
            "file changed after patch verification",
        ))
    };
    if !std::fs::symlink_metadata(path).map_err(error)?.is_file() {
        return Err(changed());
    }
    // Read and write through the same handle so a path replacement cannot
    // redirect the write after we compare the content.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(error)?;
    let mut current = Vec::new();
    (&mut file)
        .take(original.len() as u64 + 1)
        .read_to_end(&mut current)
        .map_err(error)?;
    if current != original.as_bytes() {
        return Err(changed());
    }
    if original == content {
        return Ok(false);
    }
    file.rewind().map_err(error)?;
    file.write_all(content.as_bytes()).map_err(error)?;
    file.set_len(content.len() as u64).map_err(error)?;
    Ok(true)
}

fn create_parent_dirs(path: &Path) -> Result<(), IoError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|source| IoError {
            context: format!("Failed to create parent directories for {}", path.display()),
            source,
        })?;
    }
    Ok(())
}

fn create_file(path: &Path, contents: &str) -> Result<(), IoError> {
    create_parent_dirs(path)?;
    // Check again at write time: the path may have appeared after verification.
    std::fs::File::create_new(path)
        .and_then(|mut file| file.write_all(contents.as_bytes()))
        .map_err(|source| add_file_error(path, source))
}

/// Return *only* the new file contents (joined into a single `String`) after
/// applying the chunks to the file at `path`.
fn derive_new_contents_from_chunks(
    path: &Path,
    chunks: &[UpdateFileChunk],
) -> std::result::Result<AppliedPatch, ApplyPatchError> {
    let original_contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => {
            return Err(ApplyPatchError::IoError(IoError {
                context: format!("Failed to read file to update {}", path.display()),
                source: err,
            }));
        }
    };

    derive_new_contents_from_text(path, chunks, original_contents)
}

fn derive_new_contents_from_text(
    path: &Path,
    chunks: &[UpdateFileChunk],
    original_contents: String,
) -> std::result::Result<AppliedPatch, ApplyPatchError> {
    let mut original_lines: Vec<String> = original_contents.split('\n').map(String::from).collect();
    // Drop the trailing empty element that results from the final newline so
    // that line counts match the behaviour of standard `diff`.
    if original_lines.last().is_some_and(String::is_empty) {
        original_lines.pop();
    }

    let replacements = compute_replacements(&original_lines, path, chunks)?;
    let new_lines = apply_replacements(original_lines, &replacements);
    let mut new_lines = new_lines;
    if !new_lines.last().is_some_and(String::is_empty) {
        new_lines.push(String::new());
    }
    let new_contents = new_lines.join("\n");
    Ok(AppliedPatch {
        original_contents,
        new_contents,
    })
}

/// Compute a list of replacements needed to transform `original_lines` into the
/// new lines, given the patch `chunks`. Each replacement is returned as
/// `(start_index, old_len, new_lines)`.
fn compute_replacements(
    original_lines: &[String],
    path: &Path,
    chunks: &[UpdateFileChunk],
) -> std::result::Result<Vec<(usize, usize, Vec<String>)>, ApplyPatchError> {
    let mut replacements: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut line_index: usize = 0;

    for chunk in chunks {
        // If a chunk has a `change_context`, we use seek_sequence to find it, then
        // adjust our `line_index` to continue from there.
        if let Some(ctx_line) = &chunk.change_context {
            if let Some(idx) = seek_sequence::seek_sequence(
                original_lines,
                std::slice::from_ref(ctx_line),
                line_index,
                /*eof*/ false,
            ) {
                line_index = idx + 1;
            } else {
                return Err(ApplyPatchError::ComputeReplacements(format!(
                    "Failed to find context '{}' in {}",
                    ctx_line,
                    path.display()
                )));
            }
        }

        if chunk.old_lines.is_empty() {
            // Pure addition (no old lines). We'll add them at the end or just
            // before the final empty line if one exists.
            let insertion_idx = if original_lines.last().is_some_and(String::is_empty) {
                original_lines.len() - 1
            } else {
                original_lines.len()
            };
            replacements.push((insertion_idx, 0, chunk.new_lines.clone()));
            continue;
        }

        // Otherwise, try to match the existing lines in the file with the old lines
        // from the chunk. If found, schedule that region for replacement.
        // Attempt to locate the `old_lines` verbatim within the file.  In many
        // real‑world diffs the last element of `old_lines` is an *empty* string
        // representing the terminating newline of the region being replaced.
        // This sentinel is not present in `original_lines` because we strip the
        // trailing empty slice emitted by `split('\n')`.  If a direct search
        // fails and the pattern ends with an empty string, retry without that
        // final element so that modifications touching the end‑of‑file can be
        // located reliably.

        let mut pattern: &[String] = &chunk.old_lines;
        let mut found =
            seek_sequence::seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);

        let mut new_slice: &[String] = &chunk.new_lines;

        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            // Retry without the trailing empty line which represents the final
            // newline in the file.
            pattern = &pattern[..pattern.len() - 1];
            if new_slice.last().is_some_and(String::is_empty) {
                new_slice = &new_slice[..new_slice.len() - 1];
            }

            found = seek_sequence::seek_sequence(
                original_lines,
                pattern,
                line_index,
                chunk.is_end_of_file,
            );
        }

        if let Some(start_idx) = found {
            replacements.push((start_idx, pattern.len(), new_slice.to_vec()));
            line_index = start_idx + pattern.len();
        } else {
            return Err(ApplyPatchError::ComputeReplacements(format!(
                "Failed to find expected lines in {}:\n{}",
                path.display(),
                chunk.old_lines.join("\n"),
            )));
        }
    }

    replacements.sort_by_key(|(lhs_idx, _, _)| *lhs_idx);

    Ok(replacements)
}

/// Apply the `(start_index, old_len, new_lines)` replacements to `original_lines`,
/// returning the modified file contents as a vector of lines.
fn apply_replacements(
    mut lines: Vec<String>,
    replacements: &[(usize, usize, Vec<String>)],
) -> Vec<String> {
    // We must apply replacements in descending order so that earlier replacements
    // don't shift the positions of later ones.
    for (start_idx, old_len, new_segment) in replacements.iter().rev() {
        let start_idx = *start_idx;
        let old_len = *old_len;

        // Remove old lines.
        for _ in 0..old_len {
            if start_idx < lines.len() {
                lines.remove(start_idx);
            }
        }

        // Insert new lines.
        for (offset, new_line) in new_segment.iter().enumerate() {
            lines.insert(start_idx + offset, new_line.clone());
        }
    }

    lines
}

/// Intended result of a file update for apply_patch.
#[derive(Debug, Eq, PartialEq)]
pub struct ApplyPatchFileUpdate {
    unified_diff: String,
    content: String,
}

pub fn unified_diff_from_chunks(
    path: &Path,
    chunks: &[UpdateFileChunk],
) -> std::result::Result<ApplyPatchFileUpdate, ApplyPatchError> {
    unified_diff_from_chunks_with_context(path, chunks, /*context*/ 1)
}

pub fn unified_diff_from_chunks_with_context(
    path: &Path,
    chunks: &[UpdateFileChunk],
    context: usize,
) -> std::result::Result<ApplyPatchFileUpdate, ApplyPatchError> {
    let AppliedPatch {
        original_contents,
        new_contents,
    } = derive_new_contents_from_chunks(path, chunks)?;
    let text_diff = TextDiff::from_lines(&original_contents, &new_contents);
    let unified_diff = text_diff.unified_diff().context_radius(context).to_string();
    Ok(ApplyPatchFileUpdate {
        unified_diff,
        content: new_contents,
    })
}

/// Apply a pre-verified [`ApplyPatchAction`] directly to the filesystem.
///
/// Unlike [`apply_patch`] which re-parses the raw patch string, this function
/// uses the already-computed `new_content` from the action's changes. This is
/// the preferred path when the harness has already verified the patch — it
/// avoids redundant parsing and can run in a sandboxed fork without needing
/// to serialize/deserialize through a subprocess CLI.
///
/// Returns the summary string (e.g. "Success. Updated the following files:\nM src/foo.rs\n").
pub fn apply_action(action: &ApplyPatchAction) -> std::result::Result<String, ApplyPatchError> {
    if action.is_empty() {
        return Err(empty_patch());
    }

    let mut affected = AffectedPaths::default();
    for (path, change) in action.changes() {
        affected.apply_change(path, change)?;
    }

    let mut summary = Vec::new();
    print_summary(&affected, &mut summary).map_err(|e| {
        ApplyPatchError::IoError(IoError {
            context: "Failed to write summary".to_string(),
            source: e,
        })
    })?;
    Ok(String::from_utf8_lossy(&summary).into_owned())
}

/// Print the summary of changes in git-style format.
/// Write a summary of changes to the given writer.
pub fn print_summary(
    affected: &AffectedPaths,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    if affected.added.is_empty() && affected.modified.is_empty() && affected.deleted.is_empty() {
        return writeln!(out, "Success. No files changed.");
    }
    writeln!(out, "Success. Updated the following files:")?;
    for path in &affected.added {
        writeln!(out, "A {}", path.display())?;
    }
    for path in &affected.modified {
        writeln!(out, "M {}", path.display())?;
    }
    for path in &affected.deleted {
        writeln!(out, "D {}", path.display())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
