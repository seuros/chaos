use super::tool::ApplyPatchWorkspace;
use anyhow::Context;
use assert_cmd::assert::Assert;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

#[test]
fn test_apply_patch_scenarios() -> anyhow::Result<()> {
    let scenarios_dir = chaos_which::find_resource!("tests/fixtures/scenarios")?;
    for scenario in fs::read_dir(&scenarios_dir)
        .with_context(|| format!("read patch scenarios at {}", scenarios_dir.display()))?
    {
        let scenario = scenario?;
        let path = scenario.path();
        if path.is_dir() {
            run_apply_patch_scenario(&path)
                .with_context(|| format!("run patch scenario {}", path.display()))?;
        }
    }
    Ok(())
}

/// Run the patch once and check its optional CLI expectations and entire resulting tree.
fn run_apply_patch_scenario(dir: &Path) -> anyhow::Result<()> {
    let workspace = ApplyPatchWorkspace::new()?;

    // Copy the input files to the temporary directory
    let input_dir = dir.join("input");
    if input_dir.is_dir() {
        copy_dir_recursive(&input_dir, workspace.root())?;
    }

    // Read the patch.txt file
    let patch = fs::read_to_string(dir.join("patch.txt"))?;

    let output = workspace.run_arg_patch(patch)?;
    check_cli_expectations(dir, Assert::new(output))?;

    // Assert that the final state matches the expected state exactly
    let expected_dir = dir.join("expected");
    let expected_snapshot = snapshot_dir(&expected_dir)?;
    let actual_snapshot = snapshot_dir(workspace.root())?;

    assert_eq!(
        actual_snapshot,
        expected_snapshot,
        "Scenario {} did not match expected final state",
        dir.display()
    );

    Ok(())
}

fn check_cli_expectations(dir: &Path, output: Assert) -> anyhow::Result<()> {
    let success = dir.join("success.stdout");
    let failure = dir.join("failure.stderr");
    let output = output.append_context("scenario", dir.display().to_string());
    match (success.try_exists()?, failure.try_exists()?) {
        (true, false) => {
            output.code(0).stdout(fs::read(success)?).stderr("");
        }
        (false, true) => {
            output.code(1).stdout("").stderr(fs::read(failure)?);
        }
        (true, true) => anyhow::bail!("specify only one of success.stdout and failure.stderr"),
        (false, false) => {} // Only the filesystem state is specified.
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    File(Vec<u8>),
    Dir,
}

fn snapshot_dir(root: &Path) -> anyhow::Result<BTreeMap<PathBuf, Entry>> {
    let mut entries = BTreeMap::new();
    if root.is_dir() {
        snapshot_dir_recursive(root, root, &mut entries)?;
    }
    Ok(entries)
}

fn snapshot_dir_recursive(
    base: &Path,
    dir: &Path,
    entries: &mut BTreeMap<PathBuf, Entry>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(stripped) = path.strip_prefix(base).ok() else {
            continue;
        };
        let rel = stripped.to_path_buf();

        // Under Buck2, files in `__srcs` are often materialized as symlinks.
        // Use `metadata()` (follows symlinks) so our fixture snapshots work
        // under both Cargo and Buck2.
        let metadata = fs::metadata(&path)?;
        if metadata.is_dir() {
            entries.insert(rel, Entry::Dir);
            snapshot_dir_recursive(base, &path, entries)?;
        } else if metadata.is_file() {
            let contents = fs::read(&path)?;
            entries.insert(rel, Entry::File(contents));
        }
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest_path = dst.join(entry.file_name());

        // See note in `snapshot_dir_recursive` about Buck2 symlink trees.
        let metadata = fs::metadata(&path)?;
        if metadata.is_dir() {
            fs::create_dir_all(&dest_path)?;
            copy_dir_recursive(&path, &dest_path)?;
        } else if metadata.is_file() {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&path, &dest_path)?;
        }
    }
    Ok(())
}
