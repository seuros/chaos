#![allow(clippy::expect_used, clippy::unwrap_used)]

use assert_cmd::prelude::*;
use chaos_diff::CHAOS_CORE_APPLY_PATCH_ARG1;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

/// While we may add an `apply-patch` subcommand to the `chaos` CLI multitool
/// at some point, we must ensure that the main binary can still emulate the
/// `apply_patch` CLI through the secret exec entrypoint.
#[test]
fn test_standalone_exec_cli_can_use_apply_patch() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let relative_path = "source.txt";
    let absolute_path = tmp.path().join(relative_path);
    fs::write(&absolute_path, "original content\n")?;

    Command::new(chaos_which::cargo_bin("chaos")?)
        .arg(CHAOS_CORE_APPLY_PATCH_ARG1)
        .arg(
            r#"*** Begin Patch
*** Update File: source.txt
@@
-original content
+modified by apply_patch
*** End Patch"#,
        )
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout("Success. Updated the following files:\nM source.txt\n")
        .stderr(predicates::str::is_empty());
    assert_eq!(
        fs::read_to_string(absolute_path)?,
        "modified by apply_patch\n"
    );
    Ok(())
}
