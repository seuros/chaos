use super::LOCK_FILENAME;
use super::janitor_cleanup;
use std::fs;
use std::fs::File;
use std::path::Path;

fn create_lock(dir: &Path) -> std::io::Result<File> {
    let lock_path = dir.join(LOCK_FILENAME);
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
}

#[test]
fn janitor_skips_dirs_without_lock_file() -> std::io::Result<()> {
    let root = tempfile::tempdir()?;
    let dir = root.path().join("no-lock");
    fs::create_dir(&dir)?;

    janitor_cleanup(root.path())?;

    assert!(dir.exists());
    Ok(())
}

#[test]
fn janitor_skips_dirs_with_held_lock() -> std::io::Result<()> {
    let root = tempfile::tempdir()?;
    let dir = root.path().join("locked");
    fs::create_dir(&dir)?;
    let lock_file = create_lock(&dir)?;
    lock_file.try_lock()?;

    janitor_cleanup(root.path())?;

    assert!(dir.exists());
    Ok(())
}

#[test]
fn janitor_removes_dirs_with_unlocked_lock() -> std::io::Result<()> {
    let root = tempfile::tempdir()?;
    let dir = root.path().join("stale");
    fs::create_dir(&dir)?;
    create_lock(&dir)?;

    janitor_cleanup(root.path())?;

    assert!(!dir.exists());
    Ok(())
}
