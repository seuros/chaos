//! Explicitly privileged UI fixture. Never invokes sudo or loads a module.

use anyhow::{Context, Result, ensure};
use std::collections::HashMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};

pub(super) struct WardenLaunch {
    helper: String,
    user: String,
    pub(super) journal: PathBuf,
    pub(super) uid: libc::uid_t,
    pub(super) gid: libc::gid_t,
}

impl WardenLaunch {
    pub(super) fn from_test_environment() -> Result<Self> {
        ensure!(
            unsafe { libc::geteuid() } == 0,
            "explicit root-only fixture"
        );
        let helper = std::env::var("CHAOS_FREEBSD_TEST_HELPER")?;
        ensure!(
            Path::new(&helper).is_absolute() && Path::new(&helper).is_file(),
            "expected an absolute helper executable"
        );
        // Do not use cargo_bin's auto-build fallback in a root-only fixture.
        let journal =
            PathBuf::from(std::env::var_os("CHAOS_FREEBSD_TEST_JOURNAL").context(
                "set CHAOS_FREEBSD_TEST_JOURNAL to the freshly built journal executable",
            )?);
        ensure!(
            journal.is_absolute() && journal.is_file(),
            "expected an absolute journal executable"
        );
        let user = std::env::var("CHAOS_FREEBSD_TEST_USER")?;
        let name = CString::new(user.as_str())?;
        let mut storage = vec![0_u8; 64 * 1024];
        let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut found = std::ptr::null_mut();
        let result = unsafe {
            libc::getpwnam_r(
                name.as_ptr(),
                record.as_mut_ptr(),
                storage.as_mut_ptr().cast(),
                storage.len(),
                &mut found,
            )
        };
        ensure!(result == 0 && !found.is_null(), "cannot resolve test user");
        let record = unsafe { record.assume_init() };
        ensure!(record.pw_uid != 0, "test user must not be root");
        Ok(Self {
            helper,
            user,
            journal,
            uid: record.pw_uid,
            gid: record.pw_gid,
        })
    }

    pub(super) fn give_test_home_to_user(&self, path: &Path) -> Result<()> {
        // Only our fresh, private fixture tree, before starting any child.
        // Refuse symlinks rather than following anything outside that tree.
        let metadata = path.symlink_metadata()?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "unexpected test symlink"
        );
        if metadata.is_dir() {
            for entry in path.read_dir()? {
                self.give_test_home_to_user(&entry?.path())?;
            }
        }
        std::os::unix::fs::chown(path, Some(self.uid), Some(self.gid))
            .with_context(|| format!("assign fixture path {}", path.display()))?;
        Ok(())
    }

    pub(super) fn console_command(
        self,
        main: &str,
        args: Vec<String>,
        environment: &HashMap<String, String>,
    ) -> (String, Vec<String>) {
        let mut wrapped = vec![
            "--warden-launch".into(),
            "--user".into(),
            self.user,
            "--".into(),
            "/usr/bin/env".into(),
        ];
        // The production launcher correctly discards root's environment.
        // Set fixture isolation only AFTER its identity drop, preserving the
        // private bootstrap FD until the real main captures it before threads.
        wrapped.extend(
            environment
                .iter()
                .map(|(key, value)| format!("{key}={value}")),
        );
        wrapped.extend([
            main.into(),
            "--sandbox".into(),
            "read-only".into(),
            "--ask-for-approval".into(),
            "headless".into(),
        ]);
        wrapped.extend(args);
        (self.helper, wrapped)
    }
}
