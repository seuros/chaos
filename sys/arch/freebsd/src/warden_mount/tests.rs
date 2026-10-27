//! Warden scoped-mount regressions.
use super::*;
use journal::{Event, replay};
use std::io::Cursor;
use std::path::Path;

fn record(number: i32) -> MountRecord {
    MountRecord {
        id: MountId([number, 42]),
        target: PathBuf::from(format!("/private/state/mount-{number}")),
        filesystem_type: "nullfs".to_owned(),
        mounted_from: PathBuf::from("/source"),
        flags: libc::MNT_RDONLY as u64,
    }
}
fn log(events: &[Event]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for event in events {
        bytes.extend(serde_json::to_vec(event).unwrap());
        bytes.push(b'\n');
    }
    bytes
}

#[test]
fn rollback_is_child_first_and_busy_child_preserves_parents() {
    let mut resources = vec![1, 2, 3];
    let mut removed = Vec::new();
    let error = rollback(&mut resources, |id| {
        removed.push(*id);
        if *id == 2 {
            return Err(io::Error::from_raw_os_error(libc::EBUSY));
        }
        Ok(())
    })
    .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EBUSY));
    assert_eq!(removed, [3, 2]);
    assert_eq!(resources, [1, 2]);
    rollback(&mut resources, |id| {
        removed.push(*id);
        Ok(())
    })
    .unwrap();
    assert_eq!(removed, [3, 2, 2, 1]);
    assert!(resources.is_empty());
}

#[test]
fn journal_recovers_committed_and_pending_syscalls() {
    let root = Path::new("/private/state");
    let events = [
        Event::Header {
            version: 1,
            root: root.to_owned(),
        },
        Event::Before {
            filesystem_type: "nullfs".to_owned(),
            target: record(1).target,
            prior_ids: vec![],
        },
        Event::Mounted { record: record(1) },
        Event::Before {
            filesystem_type: "nullfs".to_owned(),
            target: record(2).target,
            prior_ids: vec![record(1).id],
        },
    ];
    let replayed = replay(root, Cursor::new(log(&events))).unwrap();
    assert_eq!(replayed.owned, [record(1)]);
    assert_eq!(replayed.pending.unwrap().1, [record(1).id]);
}

#[test]
fn failed_mount_does_not_become_owned_and_unmounts_are_reverse_order() {
    let root = Path::new("/private/state");
    let events = [
        Event::Header {
            version: 1,
            root: root.to_owned(),
        },
        Event::Before {
            filesystem_type: "nullfs".to_owned(),
            target: record(1).target,
            prior_ids: vec![],
        },
        Event::Mounted { record: record(1) },
        Event::Before {
            filesystem_type: "nullfs".to_owned(),
            target: record(2).target,
            prior_ids: vec![record(1).id],
        },
        Event::MountFailed,
        Event::Unmounted { id: record(1).id },
    ];
    let result = replay(root, Cursor::new(log(&events))).unwrap();
    assert!(result.owned.is_empty() && result.pending.is_none());
}

#[test]
fn malformed_scope_and_complete_corrupt_records_are_refused() {
    let root = Path::new("/private/state");
    let header = Event::Header {
        version: 1,
        root: root.to_owned(),
    };
    let mut bytes = log(&[header]);
    bytes.extend(b"corrupt complete record\n");
    assert!(replay(root, Cursor::new(bytes)).is_err());
    assert!(
        replay(
            root,
            Cursor::new(log(&[Event::Header {
                version: 1,
                root: PathBuf::from("/other"),
            }]))
        )
        .is_err()
    );
}

#[test]
fn incomplete_final_append_does_not_authorize_an_operation() {
    let root = Path::new("/private/state");
    let mut bytes = log(&[Event::Header {
        version: 1,
        root: root.to_owned(),
    }]);
    bytes.extend(b"{\"event\":\"before\"");
    let replayed = replay(root, Cursor::new(bytes)).unwrap();
    assert!(replayed.owned.is_empty() && replayed.pending.is_none());
}

#[test]
fn journal_rejects_lexical_escape_and_a_different_commit_target() {
    let root = Path::new("/private/state");
    let header = Event::Header {
        version: 1,
        root: root.to_owned(),
    };
    for target in [
        "/private/state/../unrelated",
        "/private/state",
        "/private/state-elsewhere/view",
    ] {
        let before = Event::Before {
            filesystem_type: "nullfs".into(),
            target: PathBuf::from(target),
            prior_ids: vec![],
        };
        assert!(replay(root, Cursor::new(log(&[header.clone(), before]))).is_err());
    }
    let before = Event::Before {
        filesystem_type: "nullfs".into(),
        target: record(1).target,
        prior_ids: vec![],
    };
    assert!(
        replay(
            root,
            Cursor::new(log(&[header, before, Event::Mounted { record: record(2) }]))
        )
        .is_err()
    );
}

#[test]
fn native_mount_snapshot_is_read_only_and_has_unique_ids() {
    let table = native::snapshot().unwrap();
    assert!(table.iter().any(|m| m.target == Path::new("/")));
    let mut ids: Vec<_> = table.iter().map(|m| m.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), table.len());
}

#[test]
fn directory_pins_reject_symlinks_in_every_component_and_keep_the_opened_ancestor() {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, symlink};

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let original = root.join("original");
    let moved = root.join("moved");
    let outside = root.join("outside");
    std::fs::create_dir_all(original.join("child")).unwrap();
    std::fs::create_dir_all(outside.join("child")).unwrap();
    std::fs::write(root.join("file"), "not a directory").unwrap();
    let ancestor = native::pin_absolute_directory(&original).unwrap();
    let expected = std::fs::metadata(original.join("child")).unwrap();
    std::fs::rename(&original, &moved).unwrap();
    symlink(&outside, &original).unwrap();
    symlink(&moved, root.join("alias")).unwrap();
    let nul = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/nul\0/path"));
    for path in [
        original.join("child"),
        root.join("alias"),
        root.join("file"),
        root.join("moved/../outside"),
        PathBuf::from("relative"),
        nul,
    ] {
        assert!(
            native::pin_absolute_directory(&path).is_err(),
            "accepted unpinned or invalid path {path:?}"
        );
    }
    let child = native::pin_relative_directory(ancestor, Path::new("child")).unwrap();
    let actual = child.metadata().unwrap();
    assert_eq!(
        (actual.dev(), actual.ino()),
        (expected.dev(), expected.ino())
    );
    assert_ne!(
        unsafe { libc::fcntl(child.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
        0
    );
    for path in ["/absolute", "../outside", "child/../../outside"] {
        assert!(
            native::pin_relative_directory(
                native::pin_absolute_directory(&moved).unwrap(),
                Path::new(path)
            )
            .is_err()
        );
    }
}

#[test]
#[ignore = "explicit root-only stock-filesystem test; creates temporary mounts, no jail/module"]
fn native_scoped_mounts_are_pinned_readonly_and_cleanly_removed() {
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "run the built test binary explicitly as root"
    );
    let source = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("marker"), "original").unwrap();
    let mut mounts = ScopedMounts::create(Path::new("/tmp")).unwrap();
    let state = mounts.state_path().to_owned();
    mounts.nullfs(source.path(), Path::new(""), true).unwrap();
    assert_eq!(mounts.records().len(), 2);
    assert_eq!(
        std::fs::read_to_string(mounts.view_path().join("marker")).unwrap(),
        "original"
    );
    let error = std::fs::write(mounts.view_path().join("marker"), "changed").unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EROFS));
    let pin = std::fs::File::open(mounts.view_path().join("marker")).unwrap();
    assert_eq!(
        mounts.cleanup().unwrap_err().raw_os_error(),
        Some(libc::EBUSY)
    );
    assert_eq!(
        mounts.records().len(),
        2,
        "busy child must preserve its control mount"
    );
    drop(pin);
    mounts.cleanup().unwrap();
    assert!(!state.exists());
    assert_eq!(
        std::fs::read_to_string(source.path().join("marker")).unwrap(),
        "original"
    );
}

#[test]
#[ignore = "explicit root-only private-devfs test; requires existing hide-only ruleset 1"]
fn native_minimal_devfs_preserves_host_rules_and_hides_management_devices() {
    use std::io::Write;
    let source = tempfile::tempdir().unwrap();
    std::fs::create_dir(source.path().join("dev")).unwrap();
    let host = std::process::Command::new("/sbin/devfs")
        .args(["rule", "-s", "1", "show"])
        .output()
        .unwrap();
    assert!(host.status.success());
    let mut mounts = ScopedMounts::create(Path::new("/tmp")).unwrap();
    let state = mounts.state_path().to_owned();
    mounts.nullfs(source.path(), Path::new(""), true).unwrap();
    mounts.minimal_devices(Path::new("dev")).unwrap();
    let mut null = std::fs::OpenOptions::new()
        .write(true)
        .open(mounts.view_path().join("dev/null"))
        .unwrap();
    null.write_all(b"minimal devices work").unwrap();
    drop(null);
    let after = std::process::Command::new("/sbin/devfs")
        .args(["rule", "-s", "1", "show"])
        .output()
        .unwrap();
    assert!(after.status.success());
    assert_eq!(host.stdout, after.stdout);
    assert!(!mounts.view_path().join("dev/zfs").exists());
    assert!(!mounts.view_path().join("dev/fd").exists());
    mounts.cleanup().unwrap();
    assert!(!state.exists());
}

#[test]
#[ignore = "explicit root-only stock-filesystem test; tests guarded EDEADLK, no jail/module"]
fn native_same_source_carveout_is_refused_without_forcing_or_losing_cleanup() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let source = tempfile::tempdir().unwrap();
    std::fs::create_dir(source.path().join("protected")).unwrap();
    let mut mounts = ScopedMounts::create(Path::new("/tmp")).unwrap();
    let state = mounts.state_path().to_owned();
    mounts.nullfs(source.path(), Path::new(""), false).unwrap();
    let error = mounts
        .nullfs(
            &source.path().join("protected"),
            Path::new("protected"),
            true,
        )
        .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EDEADLK));
    assert_eq!(mounts.records().len(), 2);
    mounts.cleanup().unwrap();
    assert!(!state.exists());
}

#[test]
#[ignore = "VM ONLY: observed WITNESS reversal; requires CHAOS_FREEBSD_EXPERIMENTAL_MOUNT_STACK=1"]
fn experimental_independent_tmpfs_destinations_preserve_live_views_and_recover() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert_eq!(
        std::env::var_os("CHAOS_FREEBSD_EXPERIMENTAL_MOUNT_STACK").as_deref(),
        Some(std::ffi::OsStr::new("1")),
        "quarantined WITNESS reproducer; run only in a disposable VM"
    );
    // Do not replace the direct EDEADLK regression above. Each nullfs mount in
    // this experiment covers a fresh tmpfs root, not an alias of its own lower
    // vnode. File operations passed, BUT teardown triggered WITNESS's
    // tmpfs->zfs / zfs->tmpfs reversal. Keep this OUT of the native_ test batch
    // and production until kernel review; syscall success is not safety proof.
    for recover in [false, true] {
        let source = tempfile::tempdir().unwrap();
        let workspace = source.path().join("workspace");
        let protected = workspace.join("protected");
        std::fs::create_dir_all(&protected).unwrap();
        std::fs::write(protected.join("data"), "original").unwrap();
        let mut mounts = ScopedMounts::create(Path::new("/tmp")).unwrap();
        let state = mounts.state_path().to_owned();
        mounts.nullfs(source.path(), Path::new(""), true).unwrap();
        eprintln!("independent-destination probe: writable root, recover={recover}");
        mounts.empty_mask(Path::new("workspace")).unwrap();
        mounts
            .nullfs(&workspace, Path::new("workspace"), false)
            .unwrap();
        eprintln!("independent-destination probe: protected child");
        mounts.empty_mask(Path::new("workspace/protected")).unwrap();
        mounts
            .nullfs(&protected, Path::new("workspace/protected"), true)
            .unwrap();
        let view = mounts.view_path().join("workspace");
        let protected_view = view.join("protected");
        assert_eq!(mounts.records().len(), 6);
        assert_ne!(
            mounts.records()[3].flags & libc::MNT_RDONLY as u64,
            libc::MNT_RDONLY as u64
        );
        assert_ne!(mounts.records()[5].flags & libc::MNT_RDONLY as u64, 0);
        assert_eq!(
            std::fs::read_to_string(protected_view.join("data")).unwrap(),
            "original"
        );
        // Data and directory-entry changes made on the host remain live.
        std::fs::write(protected.join("data"), "host-update").unwrap();
        std::fs::write(protected.join("new-entry"), "new").unwrap();
        assert_eq!(
            std::fs::read_to_string(protected_view.join("data")).unwrap(),
            "host-update"
        );
        assert_eq!(
            std::fs::read_to_string(protected_view.join("new-entry")).unwrap(),
            "new"
        );
        for index in 0..64 {
            let name = format!("work-{index}");
            let renamed = format!("renamed-{index}");
            std::fs::write(view.join(&name), "live-write").unwrap();
            assert_eq!(
                std::fs::read_to_string(workspace.join(&name)).unwrap(),
                "live-write"
            );
            std::fs::rename(view.join(name), view.join(&renamed)).unwrap();
            assert!(workspace.join(&renamed).exists());
            std::fs::remove_file(view.join(renamed)).unwrap();
            assert_eq!(
                std::fs::canonicalize(protected_view.join("..")).unwrap(),
                view
            );
            assert!(std::fs::read_dir(&protected_view).unwrap().count() >= 2);
            let error = std::fs::write(protected_view.join("data"), "forbidden").unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::EROFS));
        }
        let error = std::fs::rename(&protected_view, view.join("moved")).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EBUSY));
        let error = std::fs::remove_file(protected_view.join("data")).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EROFS));
        assert_eq!(
            std::fs::read_to_string(protected.join("data")).unwrap(),
            "host-update"
        );
        eprintln!("independent-destination probe: teardown");
        if recover {
            let retained = mounts.leave_for_recovery();
            ScopedMounts::recover(&retained).unwrap();
        } else {
            mounts.cleanup().unwrap();
        }
        assert!(!state.exists());
    }
}

#[test]
#[ignore = "explicit root-only stock-filesystem recovery test; no jail/module"]
fn native_recovery_reconciles_syscall_without_commit_record() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let source = tempfile::tempdir().unwrap();
    let mut mounts = ScopedMounts::create(Path::new("/tmp")).unwrap();
    mounts.nullfs(source.path(), Path::new(""), true).unwrap();
    let state = mounts.leave_for_recovery();
    let journal = state.join("mounts.jsonl");
    let bytes = std::fs::read(&journal).unwrap();
    let preceding = bytes[..bytes.len() - 1]
        .iter()
        .rposition(|b| *b == b'\n')
        .unwrap()
        + 1;
    std::fs::write(&journal, &bytes[..preceding]).unwrap();
    ScopedMounts::recover(&state).unwrap();
    assert!(!state.exists());
}
