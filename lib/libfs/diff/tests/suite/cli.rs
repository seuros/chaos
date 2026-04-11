use super::tool::ApplyPatchWorkspace;
use assert_cmd::assert::Assert;
use std::fs;

fn add_file_patch(file: &str, contents: &str) -> String {
    format!(
        r#"*** Begin Patch
*** Add File: {file}
+{contents}
*** End Patch"#
    )
}

fn update_file_patch(file: &str, from: &str, to: &str) -> String {
    format!(
        r#"*** Begin Patch
*** Update File: {file}
@@
-{from}
+{to}
*** End Patch"#
    )
}

fn success_output(status: char, file: &str) -> String {
    format!("Success. Updated the following files:\n{status} {file}\n")
}

fn exercise_add_and_update(
    file: &str,
    mut apply_patch: impl FnMut(&ApplyPatchWorkspace, String) -> anyhow::Result<Assert>,
) -> anyhow::Result<()> {
    let workspace = ApplyPatchWorkspace::new()?;
    let absolute_path = workspace.path(file);

    apply_patch(&workspace, add_file_patch(file, "hello"))?
        .success()
        .stdout(success_output('A', file));
    assert_eq!(fs::read_to_string(&absolute_path)?, "hello\n");

    apply_patch(&workspace, update_file_patch(file, "hello", "world"))?
        .success()
        .stdout(success_output('M', file));
    assert_eq!(fs::read_to_string(&absolute_path)?, "world\n");

    Ok(())
}

#[test]
fn test_apply_patch_cli_add_and_update() -> anyhow::Result<()> {
    exercise_add_and_update("cli_test.txt", |workspace, patch| {
        workspace.assert_arg_patch(patch)
    })
}

#[test]
fn test_apply_patch_cli_stdin_add_and_update() -> anyhow::Result<()> {
    exercise_add_and_update("cli_test_stdin.txt", |workspace, patch| {
        workspace.assert_stdin_patch(patch)
    })
}
