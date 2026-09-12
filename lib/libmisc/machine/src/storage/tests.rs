use super::*;
use std::os::unix::fs::symlink;

fn target(role: StorageRole, path: impl Into<PathBuf>) -> StorageTarget {
    StorageTarget {
        role,
        path: path.into(),
    }
}

#[test]
fn only_explicit_paths_are_probed_and_shared_filesystems_are_deduplicated() -> io::Result<()> {
    assert!(inspect_storage(&[]).filesystems.is_empty());
    let dir = tempfile::tempdir()?;
    let snapshot = inspect_storage(&[
        target(StorageRole::Workspace, dir.path()),
        target(
            StorageRole::Output,
            dir.path().join("not-created/image.bin"),
        ),
        target(StorageRole::State, dir.path()),
        target(StorageRole::State, dir.path()),
    ]);
    assert!(
        snapshot.unavailable.is_empty(),
        "{:?}",
        snapshot.unavailable
    );
    assert_eq!(snapshot.filesystems.len(), 1);
    let filesystem = &snapshot.filesystems[0];
    assert_eq!(filesystem.targets.len(), 3);
    for resolved in &filesystem.targets {
        assert_eq!(resolved.probed_path, fs::canonicalize(dir.path())?);
    }
    assert!(filesystem.total_bytes > 0);
    Ok(())
}

#[test]
fn symlinked_output_uses_the_destination_not_the_lexical_parent() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("destination");
    fs::create_dir(&destination)?;
    symlink(&destination, dir.path().join("link"))?;
    let output = target(StorageRole::Output, dir.path().join("link/new/file.bin"));
    let snapshot = inspect_storage(std::slice::from_ref(&output));
    assert!(snapshot.unavailable.is_empty());
    let resolved = &snapshot.filesystems[0].targets[0];
    assert_eq!(resolved.target, output);
    assert_eq!(resolved.probed_path, fs::canonicalize(destination)?);
    Ok(())
}

#[test]
fn failed_resolution_never_silently_uses_a_different_filesystem() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    symlink(dir.path().join("absent"), dir.path().join("dangling"))?;
    symlink(dir.path().join("loop"), dir.path().join("loop"))?;
    fs::write(dir.path().join("file"), "")?;
    let paths = [
        PathBuf::from("relative"),
        dir.path().join("dangling"),
        dir.path().join("dangling/output"),
        dir.path().join("loop/output"),
        dir.path().join("file/output"),
        dir.path().join("absent/../output"),
    ];
    let targets: Vec<_> = paths
        .into_iter()
        .map(|path| target(StorageRole::Output, path))
        .collect();
    let snapshot = inspect_storage(&targets);
    assert!(snapshot.filesystems.is_empty());
    assert_eq!(snapshot.unavailable.len(), targets.len());
    Ok(())
}

#[test]
fn existing_parent_traversal_is_resolved_by_the_filesystem() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    fs::create_dir(dir.path().join("child"))?;
    assert_eq!(
        existing_path(&dir.path().join("child/.."))?,
        fs::canonicalize(dir.path())?,
    );
    Ok(())
}

fn filesystem(device: u64, available_bytes: u64, read_only: bool) -> Filesystem {
    Filesystem {
        id: FilesystemId {
            device,
            filesystem: device,
        },
        filesystem_type: Some("test".into()),
        backing: FilesystemBacking::Other,
        total_bytes: 100,
        available_bytes,
        available_inodes: None,
        read_only,
        targets: Vec::new(),
    }
}

#[test]
fn grouping_is_by_filesystem_and_access_view_not_role_or_global_free_space() {
    let mut snapshot = inspect_storage(&[]);
    for (path, device, free, read_only) in [
        ("/workspace", 1, 50, false),
        ("/archive-output", 2, 0, false),
        ("/read-only-view", 1, 50, true),
    ] {
        snapshot.insert(
            ResolvedStorageTarget {
                target: target(StorageRole::Output, path),
                probed_path: PathBuf::from(path),
            },
            filesystem(device, free, read_only),
        );
    }
    assert_eq!(snapshot.filesystems.len(), 3);
    assert_eq!(snapshot.filesystems[0].available_percent(), Some(50.0));
    assert_eq!(snapshot.filesystems[1].available_percent(), Some(0.0));
    assert!(snapshot.filesystems[2].read_only);
    assert_eq!(snapshot.filesystems[0].targets.len(), 1);
}

#[test]
fn absent_capacity_is_unknown_not_a_full_or_empty_disk() {
    let mut filesystem = filesystem(1, 5, false);
    assert_eq!(filesystem.available_percent(), Some(5.0));
    filesystem.total_bytes = 0;
    assert_eq!(filesystem.available_percent(), None);
    filesystem.total_bytes = 1;
    assert_eq!(filesystem.available_percent(), None);
}

#[test]
fn temporary_role_is_preserved_even_on_a_non_memory_filesystem() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let snapshot = inspect_storage(&[target(StorageRole::Temporary, dir.path())]);
    assert!(snapshot.unavailable.is_empty());
    assert_eq!(
        snapshot.filesystems[0].targets[0].target.role,
        StorageRole::Temporary
    );
    Ok(())
}
