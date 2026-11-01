use super::*;
use pretty_assertions::assert_eq;

fn backup_of(path: &Path) -> PathBuf {
    let mut backup = path.as_os_str().to_owned();
    backup.push(".1");
    backup.into()
}

#[test]
fn rotates_before_a_write_would_pass_the_cap() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("chaos-console.log");
    let mut file = RotatingFile::open(&path, 16)?;

    file.write_all(b"first line\n")?;
    file.write_all(b"second line\n")?;

    assert_eq!(std::fs::read_to_string(backup_of(&path))?, "first line\n");
    assert_eq!(std::fs::read_to_string(&path)?, "second line\n");
    Ok(())
}

#[test]
fn oversized_file_from_a_previous_run_rotates_on_first_write() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("chaos-console.log");
    std::fs::write(&path, "x".repeat(32))?;
    std::fs::write(backup_of(&path), "stale backup")?;
    let mut file = RotatingFile::open(&path, 16)?;

    file.write_all(b"fresh\n")?;

    assert_eq!(std::fs::read_to_string(backup_of(&path))?, "x".repeat(32));
    assert_eq!(std::fs::read_to_string(&path)?, "fresh\n");
    Ok(())
}

#[test]
fn picks_up_a_rotation_done_by_another_process() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("chaos-console.log");
    let mut file = RotatingFile::open(&path, 16)?;
    file.write_all(b"first line\n")?;
    std::fs::rename(&path, backup_of(&path))?;

    file.write_all(b"second line\n")?;

    assert_eq!(std::fs::read_to_string(&path)?, "second line\n");
    Ok(())
}

#[cfg(unix)]
#[test]
fn rotated_file_stays_owner_only() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("chaos-console.log");
    let mut file = RotatingFile::open(&path, 4)?;
    file.write_all(b"one\n")?;
    file.write_all(b"two\n")?;

    let mode = std::fs::metadata(&path)?.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    Ok(())
}
