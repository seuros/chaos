use super::*;
use chaos_ipc::protocol::{VfsAccessMode, VfsEntry, VfsPath, VfsSpecialPath};
use std::os::unix::fs::symlink;

fn source(path: &Path) -> MemorySource {
    MemorySource::File {
        path: path.into(),
        fragment: Some("section-2".into()),
    }
}

fn policy() -> VfsPolicy {
    VfsPolicy::restricted(vec![VfsEntry {
        path: VfsPath::Special {
            value: VfsSpecialPath::CurrentWorkingDirectory,
        },
        access: VfsAccessMode::Read,
    }])
}

fn directory() -> std::io::Result<tempfile::TempDir> {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)
}

#[test]
fn reads_only_currently_allowed_sources_and_preserves_the_hint() {
    let home = directory().unwrap();
    let path = home.path().join("source.md");
    std::fs::write(&path, "hello 🦀").unwrap();
    let output = read(&source(&path), &policy(), home.path()).unwrap();
    let output: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["content"], "hello 🦀");
    assert_eq!(output["fragment"], "section-2");
    assert!(read(&source(&path), &VfsPolicy::restricted(vec![]), home.path()).is_err());
}

#[test]
fn symlink_cannot_escape_the_readable_checkout() {
    let home = directory().unwrap();
    let outside = directory().unwrap();
    let secret = outside.path().join("secret");
    std::fs::write(&secret, "secret").unwrap();
    let link = home.path().join("link");
    symlink(secret, &link).unwrap();
    assert!(read(&source(&link), &policy(), home.path()).is_err());
    assert!(open(&link).is_err());
}

#[test]
fn rejects_directories_binary_oversized_and_nonregular_files() {
    let home = directory().unwrap();
    assert!(read(&source(home.path()), &policy(), home.path()).is_err());
    let path = home.path().join("source");
    for bytes in [
        vec![255],
        vec![b'x'; SOURCE_BYTES as usize + 1],
        vec![0; SOURCE_BYTES as usize],
    ] {
        std::fs::write(&path, bytes).unwrap();
        assert!(read(&source(&path), &policy(), home.path()).is_err());
    }
    std::fs::remove_file(&path).unwrap();
    let cpath = CString::new(path.as_os_str().as_bytes()).unwrap();
    // The temporary fixture owns this FIFO.
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    assert!(read(&source(&path), &policy(), home.path()).is_err());
}
