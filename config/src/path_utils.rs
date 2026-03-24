//! Filesystem path utilities shared across config crates.

use codex_utils_absolute_path::AbsolutePathBuf;
use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use tempfile::NamedTempFile;

pub struct SymlinkWritePaths {
    pub read_path: Option<PathBuf>,
    pub write_path: PathBuf,
}

/// Resolve the final filesystem target for `path` while retaining a safe write path.
///
/// This follows symlink chains (including relative symlink targets) until it reaches a
/// non-symlink path. If the chain cycles or any metadata/link resolution fails, it
/// returns `read_path: None` and uses the original absolute path as `write_path`.
/// There is no fixed max-resolution count; cycles are detected via a visited set.
pub fn resolve_symlink_write_paths(path: &Path) -> io::Result<SymlinkWritePaths> {
    let root = AbsolutePathBuf::from_absolute_path(path)
        .map(AbsolutePathBuf::into_path_buf)
        .unwrap_or_else(|_| path.to_path_buf());
    let mut current = root.clone();
    let mut visited = HashSet::new();

    loop {
        let meta = match std::fs::symlink_metadata(&current) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(SymlinkWritePaths {
                    read_path: Some(current.clone()),
                    write_path: current,
                });
            }
            Err(_) => {
                return Ok(SymlinkWritePaths {
                    read_path: None,
                    write_path: root,
                });
            }
        };

        if !meta.file_type().is_symlink() {
            return Ok(SymlinkWritePaths {
                read_path: Some(current.clone()),
                write_path: current,
            });
        }

        if !visited.insert(current.clone()) {
            return Ok(SymlinkWritePaths {
                read_path: None,
                write_path: root,
            });
        }

        let target = match std::fs::read_link(&current) {
            Ok(target) => target,
            Err(_) => {
                return Ok(SymlinkWritePaths {
                    read_path: None,
                    write_path: root,
                });
            }
        };

        let next = if target.is_absolute() {
            AbsolutePathBuf::from_absolute_path(&target)
        } else if let Some(parent) = current.parent() {
            AbsolutePathBuf::resolve_path_against_base(&target, parent)
        } else {
            return Ok(SymlinkWritePaths {
                read_path: None,
                write_path: root,
            });
        };

        let next = match next {
            Ok(path) => path.into_path_buf(),
            Err(_) => {
                return Ok(SymlinkWritePaths {
                    read_path: None,
                    write_path: root,
                });
            }
        };

        current = next;
    }
}

pub fn write_atomically(write_path: &Path, contents: &str) -> io::Result<()> {
    let parent = write_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path {} has no parent directory", write_path.display()),
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let tmp = NamedTempFile::new_in(parent)?;
    std::fs::write(tmp.path(), contents)?;
    tmp.persist(write_path)?;
    Ok(())
}
