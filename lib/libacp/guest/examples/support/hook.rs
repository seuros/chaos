//! Temporary AGY-profile policy for operator-run tests, not a ChaOS DB hook.
#![allow(clippy::expect_used)]

use serde_json::{Value, json};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub(crate) struct Hook {
    path: PathBuf,
    bytes: Vec<u8>,
    audit: tempfile::NamedTempFile,
}

impl Hook {
    pub(crate) fn install(home: &Path, executable: &Path, fixture: &Path) -> Self {
        Self::install_policy(home, executable, Some(fixture))
    }

    pub(crate) fn deny_all(home: &Path, executable: &Path) -> Self {
        Self::install_policy(home, executable, None)
    }

    fn install_policy(home: &Path, executable: &Path, fixture: Option<&Path>) -> Self {
        let metadata = std::fs::symlink_metadata(home).expect("dedicated profile");
        assert!(metadata.is_dir() && metadata.mode() & 0o077 == 0);
        // SAFETY: geteuid has no preconditions.
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
        let audit = tempfile::NamedTempFile::new().expect("private hook audit");
        for path in [Some(executable), Some(audit.path()), fixture]
            .into_iter()
            .flatten()
        {
            assert!(path.is_absolute());
            assert!(
                !path
                    .as_os_str()
                    .as_encoded_bytes()
                    .iter()
                    .any(u8::is_ascii_whitespace)
            );
        }
        let mut command = format!("{} {}", executable.display(), audit.path().display());
        if let Some(fixture) = fixture {
            command.push_str(&format!(" --allow-kernel-fixture {}", fixture.display()));
        }
        let bytes = serde_json::to_vec(&json!({
            "chaos-kernel-fixture-experiment": {
                "PreToolUse": [{
                    "matcher":"*",
                    "hooks":[{"type":"command","command":command,"timeout":10}]
                }]
            }
        }))
        .expect("hook JSON");
        let directory = home.join(".gemini/config");
        std::fs::create_dir_all(&directory).expect("dedicated hook directory");
        let path = directory.join("hooks.json");
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .expect("refuse to replace an existing operator hook");
        let hook = Self { path, bytes, audit };
        file.write_all(&hook.bytes).expect("install test hook");
        hook
    }

    pub(crate) fn observed(&self, event: &str, name: &str) -> usize {
        std::fs::read_to_string(self.audit.path())
            .expect("read name-only audit")
            .lines()
            .filter(|line| {
                let entry: Value = serde_json::from_str(line).expect("audit JSON");
                entry["event"] == event && entry["name"] == name
            })
            .count()
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        if std::fs::read(&self.path).ok().as_deref() == Some(self.bytes.as_slice()) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
