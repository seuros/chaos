use super::*;
use pretty_assertions::assert_eq;
use std::assert_matches;
use std::fs;
use std::path::PathBuf;
use std::string::ToString;
use tempfile::tempdir;

/// Helper to construct a patch with the given body.
fn wrap_patch(body: &str) -> String {
    format!("*** Begin Patch\n{body}\n*** End Patch")
}

fn strs_to_strings(strs: &[&str]) -> Vec<String> {
    strs.iter().map(ToString::to_string).collect()
}

// Test helpers to reduce repetition when building bash -lc heredoc scripts
fn args_bash(script: &str) -> Vec<String> {
    strs_to_strings(&["bash", "-lc", script])
}

fn heredoc_script(prefix: &str) -> String {
    format!(
        "{prefix}apply_patch <<'PATCH'\n*** Begin Patch\n*** Add File: foo\n+hi\n*** End Patch\nPATCH"
    )
}

fn heredoc_script_with_suffix(prefix: &str, suffix: &str) -> String {
    format!(
        "{prefix}apply_patch <<'PATCH'\n*** Begin Patch\n*** Add File: foo\n+hi\n*** End Patch\nPATCH{suffix}"
    )
}

fn expected_single_add() -> Vec<Hunk> {
    vec![Hunk::AddFile {
        path: PathBuf::from("foo"),
        contents: "hi\n".to_string(),
    }]
}

fn assert_match_args(args: Vec<String>, expected_workdir: Option<&str>) {
    match maybe_parse_apply_patch(&args) {
        MaybeApplyPatch::Body(ApplyPatchArgs { hunks, workdir, .. }) => {
            assert_eq!(workdir.as_deref(), expected_workdir);
            assert_eq!(hunks, expected_single_add());
        }
        result => panic!("expected MaybeApplyPatch::Body got {result:?}"),
    }
}

fn assert_match(script: &str, expected_workdir: Option<&str>) {
    let args = args_bash(script);
    assert_match_args(args, expected_workdir);
}

fn assert_not_match(script: &str) {
    let args = args_bash(script);
    assert_matches!(
        maybe_parse_apply_patch(&args),
        MaybeApplyPatch::NotApplyPatch
    );
}

#[test]
fn test_implicit_patch_invocations_are_errors() {
    let patch = "*** Begin Patch\n*** Add File: foo\n+hi\n*** End Patch".to_string();
    let dir = tempdir().unwrap();
    for args in [vec![patch.clone()], args_bash(&patch)] {
        assert_matches!(
            maybe_parse_apply_patch_verified(&args, dir.path()),
            MaybeApplyPatchVerified::CorrectnessError(ApplyPatchError::ImplicitInvocation)
        );
    }
}

#[test]
fn test_literal_patch_command_aliases() {
    for command in ["apply_patch", "applypatch"] {
        let args = strs_to_strings(&[
            command,
            r#"*** Begin Patch
*** Add File: foo
+hi
*** End Patch
"#,
        ]);
        assert_match_args(args, None);
    }
}

#[test]
fn test_heredoc_commands_work_in_login_and_non_login_shells() {
    for command in ["apply_patch", "applypatch"] {
        let script = heredoc_script("").replacen("apply_patch", command, 1);
        for flag in ["-lc", "-c"] {
            assert_match_args(strs_to_strings(&["bash", flag, &script]), None);
        }
    }
}

#[test]
fn test_heredoc_accepts_only_single_safe_leading_cd() {
    for (prefix, workdir) in [
        ("cd foo && ", "foo"),
        ("cd 'foo bar' && ", "foo bar"),
        ("cd \"foo bar\" && ", "foo bar"),
    ] {
        assert_match(&heredoc_script(prefix), Some(workdir));
    }
}

#[test]
fn test_heredoc_rejects_shell_operators_extra_commands_and_arguments() {
    for script in [
        heredoc_script("cd foo; "),
        heredoc_script("cd bar || "),
        heredoc_script("cd bar | "),
        heredoc_script("echo foo && "),
        heredoc_script("cd foo && cd bar && "),
        heredoc_script("cd foo bar && "),
        heredoc_script("echo foo; cd bar && "),
        heredoc_script_with_suffix("cd bar && ", " && echo done"),
        "apply_patch foo <<'PATCH'\n*** Begin Patch\n*** Add File: foo\n+hi\n*** End Patch\nPATCH"
            .to_string(),
    ] {
        assert_not_match(&script);
    }
}

#[test]
fn test_apply_patch_should_resolve_absolute_paths_in_cwd() {
    let session_dir = tempdir().unwrap();
    let relative_path = "source.txt";

    // Note that we need this file to exist for the patch to be "verified"
    // and parsed correctly.
    let session_file_path = session_dir.path().join(relative_path);
    fs::write(&session_file_path, "session directory content\n").unwrap();

    let argv = vec![
        "apply_patch".to_string(),
        r#"*** Begin Patch
*** Update File: source.txt
@@
-session directory content
+updated session directory content
*** End Patch"#
            .to_string(),
    ];

    let result = maybe_parse_apply_patch_verified(&argv, session_dir.path());

    // Verify the patch contents - as otherwise we may have pulled contents
    // from the wrong file (as we're using relative paths)
    assert_eq!(
        result,
        MaybeApplyPatchVerified::Body(ApplyPatchAction {
            changes: HashMap::from([(
                session_dir.path().join(relative_path),
                ApplyPatchFileChange::Update {
                    unified_diff: r#"@@ -1 +1 @@
-session directory content
+updated session directory content
"#
                    .to_string(),
                    move_path: None,
                    new_content: "updated session directory content\n".to_string(),
                },
            )]),
            patch: argv[1].clone(),
            cwd: session_dir.path().to_path_buf(),
        })
    );
}

#[test]
fn test_apply_patch_resolves_move_path_with_effective_cwd() {
    let session_dir = tempdir().unwrap();
    let worktree_rel = "alt";
    let worktree_dir = session_dir.path().join(worktree_rel);
    fs::create_dir_all(&worktree_dir).unwrap();

    let source_name = "old.txt";
    let dest_name = "renamed.txt";
    let source_path = worktree_dir.join(source_name);
    fs::write(&source_path, "before\n").unwrap();

    let patch = wrap_patch(&format!(
        r#"*** Update File: {source_name}
*** Move to: {dest_name}
@@
-before
+after"#
    ));

    let shell_script = format!("cd {worktree_rel} && apply_patch <<'PATCH'\n{patch}\nPATCH");
    let argv = vec!["bash".into(), "-lc".into(), shell_script];

    let result = maybe_parse_apply_patch_verified(&argv, session_dir.path());
    let action = match result {
        MaybeApplyPatchVerified::Body(action) => action,
        other => panic!("expected verified body, got {other:?}"),
    };

    assert_eq!(action.cwd, worktree_dir);

    let change = action
        .changes()
        .get(&worktree_dir.join(source_name))
        .expect("source file change present");

    match change {
        ApplyPatchFileChange::Update { move_path, .. } => {
            assert_eq!(
                move_path.as_deref(),
                Some(worktree_dir.join(dest_name).as_path())
            );
        }
        other => panic!("expected update change, got {other:?}"),
    }
}
