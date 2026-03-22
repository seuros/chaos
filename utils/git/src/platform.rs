use std::path::Path;

use crate::GitToolingError;

pub fn create_symlink(
    _source: &Path,
    link_target: &Path,
    destination: &Path,
) -> Result<(), GitToolingError> {
    use std::os::unix::fs::symlink;

    symlink(link_target, destination)?;
    Ok(())
}
