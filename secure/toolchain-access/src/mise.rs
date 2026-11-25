use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncReadExt;

#[derive(Deserialize)]
struct Install {
    version: String,
    install_path: PathBuf,
    installed: bool,
}

pub(super) async fn installed(mise: &Path, key: &str, version: &str, binary: &str) -> bool {
    const MAX_OUTPUT: u64 = 256 * 1024;
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        let mut child = tokio::process::Command::new(mise)
            .args([
                "--no-config",
                "--no-env",
                "--no-hooks",
                "ls",
                "--installed",
                "--json",
                key,
            ])
            .current_dir("/")
            .env("MISE_OFFLINE", "1")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut output = Vec::new();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing mise stdout"))?;
        stdout.take(MAX_OUTPUT + 1).read_to_end(&mut output).await?;
        if output.len() as u64 > MAX_OUTPUT || !child.wait().await?.success() {
            return Ok(false);
        }
        Ok::<_, io::Error>(
            serde_json::from_slice::<Vec<Install>>(&output).is_ok_and(|installs| {
                installs
                    .iter()
                    .any(|install| install.matches(version, binary))
            }),
        )
    })
    .await;
    matches!(result, Ok(Ok(true)))
}

impl Install {
    fn matches(&self, version: &str, binary: &str) -> bool {
        if !self.installed || self.version != version || !self.install_path.is_absolute() {
            return false;
        }
        // `mise link` can register an arbitrary existing system installation.
        // Being listed as installed does not make a linked directory mise-owned.
        if !self
            .install_path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_dir())
        {
            return false;
        }
        let Ok(root) = self.install_path.canonicalize() else {
            return false;
        };
        [root.join(binary), root.join("bin").join(binary)]
            .iter()
            .any(|path| {
                path.canonicalize().is_ok_and(|path| {
                    path.starts_with(&root)
                        && path.metadata().is_ok_and(|metadata| {
                            metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                        })
                })
            })
    }
}

#[cfg(test)]
mod tests;
