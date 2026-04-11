use assert_cmd::Command;
use assert_cmd::assert::Assert;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Output;
use tempfile::{TempDir, tempdir};

pub(crate) struct ApplyPatchWorkspace {
    tempdir: TempDir,
}

impl ApplyPatchWorkspace {
    pub(crate) fn new() -> anyhow::Result<Self> {
        Ok(Self {
            tempdir: tempdir()?,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        self.tempdir.path()
    }

    pub(crate) fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root().join(relative)
    }

    pub(crate) fn write_file(
        &self,
        relative: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> anyhow::Result<PathBuf> {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, contents)?;
        Ok(path)
    }

    pub(crate) fn create_dir_all(&self, relative: impl AsRef<Path>) -> anyhow::Result<PathBuf> {
        let path = self.path(relative);
        fs::create_dir_all(&path)?;
        Ok(path)
    }

    pub(crate) fn read_to_string(&self, relative: impl AsRef<Path>) -> anyhow::Result<String> {
        Ok(fs::read_to_string(self.path(relative))?)
    }

    pub(crate) fn command(&self) -> anyhow::Result<Command> {
        let mut cmd = Command::new(chaos_which::cargo_bin("apply_patch")?);
        cmd.current_dir(self.root());
        Ok(cmd)
    }

    pub(crate) fn assert_arg_patch(&self, patch: impl Into<String>) -> anyhow::Result<Assert> {
        Ok(self.command()?.arg(patch.into()).assert())
    }

    pub(crate) fn assert_stdin_patch(&self, patch: impl Into<String>) -> anyhow::Result<Assert> {
        Ok(self.command()?.write_stdin(patch.into()).assert())
    }

    pub(crate) fn run_arg_patch(&self, patch: impl Into<String>) -> anyhow::Result<Output> {
        Ok(
            std::process::Command::new(chaos_which::cargo_bin("apply_patch")?)
                .current_dir(self.root())
                .arg(patch.into())
                .output()?,
        )
    }
}

#[test]
fn test_apply_patch_cli_applies_multiple_operations() -> anyhow::Result<()> {
    let workspace = ApplyPatchWorkspace::new()?;
    let modify_path = workspace.write_file("modify.txt", "line1\nline2\n")?;
    let delete_path = workspace.write_file("delete.txt", "obsolete\n")?;

    let patch = "*** Begin Patch\n*** Add File: nested/new.txt\n+created\n*** Delete File: delete.txt\n*** Update File: modify.txt\n@@\n-line2\n+changed\n*** End Patch";

    workspace.assert_arg_patch(patch)?.success().stdout(
        "Success. Updated the following files:\nA nested/new.txt\nM modify.txt\nD delete.txt\n",
    );

    assert_eq!(workspace.read_to_string("nested/new.txt")?, "created\n");
    assert_eq!(fs::read_to_string(&modify_path)?, "line1\nchanged\n");
    assert!(!delete_path.exists());

    Ok(())
}

#[test]
fn test_apply_patch_cli_applies_multiple_chunks() -> anyhow::Result<()> {
    let workspace = ApplyPatchWorkspace::new()?;
    let target_path = workspace.write_file("multi.txt", "line1\nline2\nline3\nline4\n")?;

    let patch = "*** Begin Patch\n*** Update File: multi.txt\n@@\n-line2\n+changed2\n@@\n-line4\n+changed4\n*** End Patch";

    workspace
        .assert_arg_patch(patch)?
        .success()
        .stdout("Success. Updated the following files:\nM multi.txt\n");

    assert_eq!(
        fs::read_to_string(&target_path)?,
        "line1\nchanged2\nline3\nchanged4\n"
    );

    Ok(())
}

#[test]
fn test_apply_patch_cli_moves_file_to_new_directory() -> anyhow::Result<()> {
    let workspace = ApplyPatchWorkspace::new()?;
    let original_path = workspace.write_file("old/name.txt", "old content\n")?;
    let new_path = workspace.path("renamed/dir/name.txt");

    let patch = "*** Begin Patch\n*** Update File: old/name.txt\n*** Move to: renamed/dir/name.txt\n@@\n-old content\n+new content\n*** End Patch";

    workspace
        .assert_arg_patch(patch)?
        .success()
        .stdout("Success. Updated the following files:\nM renamed/dir/name.txt\n");

    assert!(!original_path.exists());
    assert_eq!(fs::read_to_string(&new_path)?, "new content\n");

    Ok(())
}

#[test]
fn test_apply_patch_cli_rejects_empty_patch() -> anyhow::Result<()> {
    ApplyPatchWorkspace::new()?
        .assert_arg_patch("*** Begin Patch\n*** End Patch")?
        .failure()
        .stderr("No files were modified.\n");

    Ok(())
}

#[test]
fn test_apply_patch_cli_reports_missing_context() -> anyhow::Result<()> {
    let workspace = ApplyPatchWorkspace::new()?;
    let target_path = workspace.write_file("modify.txt", "line1\nline2\n")?;

    workspace
        .assert_arg_patch(
            "*** Begin Patch\n*** Update File: modify.txt\n@@\n-missing\n+changed\n*** End Patch",
        )?
        .failure()
        .stderr("Failed to find expected lines in modify.txt:\nmissing\n");
    assert_eq!(fs::read_to_string(&target_path)?, "line1\nline2\n");

    Ok(())
}

#[test]
fn test_apply_patch_cli_rejects_missing_file_delete() -> anyhow::Result<()> {
    ApplyPatchWorkspace::new()?
        .assert_arg_patch("*** Begin Patch\n*** Delete File: missing.txt\n*** End Patch")?
        .failure()
        .stderr("Failed to delete file missing.txt\n");

    Ok(())
}

#[test]
fn test_apply_patch_cli_rejects_empty_update_hunk() -> anyhow::Result<()> {
    ApplyPatchWorkspace::new()?
        .assert_arg_patch("*** Begin Patch\n*** Update File: foo.txt\n*** End Patch")?
        .failure()
        .stderr("Invalid patch hunk on line 2: Update file hunk for path 'foo.txt' is empty\n");

    Ok(())
}

#[test]
fn test_apply_patch_cli_requires_existing_file_for_update() -> anyhow::Result<()> {
    ApplyPatchWorkspace::new()?
        .assert_arg_patch(
            "*** Begin Patch\n*** Update File: missing.txt\n@@\n-old\n+new\n*** End Patch",
        )?
        .failure()
        .stderr(
            "Failed to read file to update missing.txt: No such file or directory (os error 2)\n",
        );

    Ok(())
}

#[test]
fn test_apply_patch_cli_move_overwrites_existing_destination() -> anyhow::Result<()> {
    let workspace = ApplyPatchWorkspace::new()?;
    let original_path = workspace.write_file("old/name.txt", "from\n")?;
    let destination = workspace.write_file("renamed/dir/name.txt", "existing\n")?;

    workspace
        .assert_arg_patch(
        "*** Begin Patch\n*** Update File: old/name.txt\n*** Move to: renamed/dir/name.txt\n@@\n-from\n+new\n*** End Patch",
        )?
        .success()
        .stdout("Success. Updated the following files:\nM renamed/dir/name.txt\n");

    assert!(!original_path.exists());
    assert_eq!(fs::read_to_string(&destination)?, "new\n");

    Ok(())
}

#[test]
fn test_apply_patch_cli_add_overwrites_existing_file() -> anyhow::Result<()> {
    let workspace = ApplyPatchWorkspace::new()?;
    let path = workspace.write_file("duplicate.txt", "old content\n")?;

    workspace
        .assert_arg_patch(
            "*** Begin Patch\n*** Add File: duplicate.txt\n+new content\n*** End Patch",
        )?
        .success()
        .stdout("Success. Updated the following files:\nA duplicate.txt\n");

    assert_eq!(fs::read_to_string(&path)?, "new content\n");

    Ok(())
}

#[test]
fn test_apply_patch_cli_delete_directory_fails() -> anyhow::Result<()> {
    let workspace = ApplyPatchWorkspace::new()?;
    workspace.create_dir_all("dir")?;

    workspace
        .assert_arg_patch("*** Begin Patch\n*** Delete File: dir\n*** End Patch")?
        .failure()
        .stderr("Failed to delete file dir\n");

    Ok(())
}

#[test]
fn test_apply_patch_cli_rejects_invalid_hunk_header() -> anyhow::Result<()> {
    ApplyPatchWorkspace::new()?
        .assert_arg_patch("*** Begin Patch\n*** Frobnicate File: foo\n*** End Patch")?
        .failure()
        .stderr("Invalid patch hunk on line 2: '*** Frobnicate File: foo' is not a valid hunk header. Valid hunk headers: '*** Add File: {path}', '*** Delete File: {path}', '*** Update File: {path}'\n");

    Ok(())
}

#[test]
fn test_apply_patch_cli_updates_file_appends_trailing_newline() -> anyhow::Result<()> {
    let workspace = ApplyPatchWorkspace::new()?;
    let target_path = workspace.write_file("no_newline.txt", "no newline at end")?;

    workspace
        .assert_arg_patch(
            "*** Begin Patch\n*** Update File: no_newline.txt\n@@\n-no newline at end\n+first line\n+second line\n*** End Patch",
        )?
        .success()
        .stdout("Success. Updated the following files:\nM no_newline.txt\n");

    let contents = fs::read_to_string(&target_path)?;
    assert!(contents.ends_with('\n'));
    assert_eq!(contents, "first line\nsecond line\n");

    Ok(())
}

#[test]
fn test_apply_patch_cli_failure_after_partial_success_leaves_changes() -> anyhow::Result<()> {
    let workspace = ApplyPatchWorkspace::new()?;
    let new_file = workspace.path("created.txt");

    workspace
        .assert_arg_patch(
            "*** Begin Patch\n*** Add File: created.txt\n+hello\n*** Update File: missing.txt\n@@\n-old\n+new\n*** End Patch",
        )?
        .failure()
        .stdout("")
        .stderr("Failed to read file to update missing.txt: No such file or directory (os error 2)\n");

    assert_eq!(fs::read_to_string(&new_file)?, "hello\n");

    Ok(())
}
