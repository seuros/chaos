//! Shell command parsing — public facade over the focused submodules.

mod ast;
mod lexer;
mod parser;

pub use lexer::shlex_join;
pub use parser::{extract_shell_command, is_small_formatting_command, parse_command_impl};

use chaos_ipc::parse_command::ParsedCommand;
use parser::single_unknown_for_command;

/// DO NOT REVIEW THIS CODE BY HAND
/// This parsing code is quite complex and not easy to hand-modify.
/// The easiest way to iterate is to add unit tests and have Chaos fix the implementation.
/// To encourage this, the tests have been put directly below this function rather than at the bottom of the file.
///
/// Parses metadata out of an arbitrary command.
/// These commands are model driven and could include just about anything.
/// The parsing is slightly lossy due to the ~infinite expressiveness of an arbitrary command.
/// The goal of the parsed metadata is to be able to provide the user with a human readable gist
/// of what it is doing.
pub fn parse_command(command: &[String]) -> Vec<ParsedCommand> {
    // Parse and then collapse consecutive duplicate commands to avoid redundant summaries.
    let parsed = parse_command_impl(command);
    let mut deduped: Vec<ParsedCommand> = Vec::with_capacity(parsed.len());
    for cmd in parsed.into_iter() {
        if deduped.last().is_some_and(|prev| prev == &cmd) {
            continue;
        }
        deduped.push(cmd);
    }
    if deduped
        .iter()
        .any(|cmd| matches!(cmd, ParsedCommand::Unknown { .. }))
    {
        vec![single_unknown_for_command(command)]
    } else {
        deduped
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
/// Tests are at the top to encourage using TDD + Chaos to fix the implementation.
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use shlex::split as shlex_split;
    use std::path::PathBuf;
    use std::string::ToString;

    fn shlex_split_safe(s: &str) -> Vec<String> {
        shlex_split(s).unwrap_or_else(|| s.split_whitespace().map(ToString::to_string).collect())
    }

    fn vec_str(args: &[&str]) -> Vec<String> {
        args.iter().map(ToString::to_string).collect()
    }

    fn assert_parsed(args: &[String], expected: Vec<ParsedCommand>) {
        let out = parse_command(args);
        assert_eq!(out, expected);
    }

    #[test]
    fn git_status_is_unknown() {
        assert_parsed(
            &vec_str(&["git", "status"]),
            vec![ParsedCommand::Unknown {
                cmd: "git status".to_string(),
            }],
        );
    }

    #[test]
    fn supports_git_grep_and_ls_files() {
        assert_parsed(
            &shlex_split_safe("git grep TODO src"),
            vec![ParsedCommand::Search {
                cmd: "git grep TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("git grep -l TODO src"),
            vec![ParsedCommand::Search {
                cmd: "git grep -l TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("git ls-files"),
            vec![ParsedCommand::ListFiles {
                cmd: "git ls-files".to_string(),
                path: None,
            }],
        );
        assert_parsed(
            &shlex_split_safe("git ls-files src"),
            vec![ParsedCommand::ListFiles {
                cmd: "git ls-files src".to_string(),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("git ls-files --exclude target src"),
            vec![ParsedCommand::ListFiles {
                cmd: "git ls-files --exclude target src".to_string(),
                path: Some("src".to_string()),
            }],
        );
    }

    #[test]
    fn handles_git_pipe_wc() {
        let inner = "git status | wc -l";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Unknown {
                cmd: inner.to_string(),
            }],
        );
    }

    #[test]
    fn bash_lc_redirect_not_quoted() {
        let inner = "echo foo > bar";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Unknown {
                cmd: "echo foo > bar".to_string(),
            }],
        );
    }

    #[test]
    fn handles_complex_bash_command_head() {
        let inner =
            "rg --version && node -v && pnpm -v && rg --files | wc -l && rg --files | head -n 40";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Unknown {
                cmd: inner.to_string(),
            }],
        );
    }

    #[test]
    fn supports_searching_for_navigate_to_route() -> anyhow::Result<()> {
        let inner = "rg -n \"navigate-to-route\" -S";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Search {
                cmd: "rg -n navigate-to-route -S".to_string(),
                query: Some("navigate-to-route".to_string()),
                path: None,
            }],
        );
        Ok(())
    }

    #[test]
    fn handles_complex_bash_command() {
        let inner = "rg -n \"BUG|FIXME|TODO|XXX|HACK\" -S | head -n 200";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Search {
                cmd: "rg -n 'BUG|FIXME|TODO|XXX|HACK' -S".to_string(),
                query: Some("BUG|FIXME|TODO|XXX|HACK".to_string()),
                path: None,
            }],
        );
    }

    #[test]
    fn supports_rg_files_with_path_and_pipe() {
        let inner = "rg --files webview/src | sed -n";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::ListFiles {
                cmd: "rg --files webview/src".to_string(),
                path: Some("webview".to_string()),
            }],
        );
    }

    #[test]
    fn supports_rg_files_then_head() {
        let inner = "rg --files | head -n 50";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::ListFiles {
                cmd: "rg --files".to_string(),
                path: None,
            }],
        );
    }

    #[test]
    fn keeps_mutating_xargs_pipeline() {
        let inner = r#"rg -l QkBindingController presentation/src/main/java | xargs perl -pi -e 's/QkBindingController/QkController/g'"#;
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Unknown {
                cmd: inner.to_string(),
            }],
        );
    }

    #[test]
    fn collapses_plain_pipeline_when_any_stage_is_unknown() {
        let command = shlex_split_safe(
            "rg -l QkBindingController presentation/src/main/java | xargs perl -pi -e 's/QkBindingController/QkController/g'",
        );
        assert_parsed(
            &command,
            vec![ParsedCommand::Unknown {
                cmd: shlex_join(&command),
            }],
        );
    }

    #[test]
    fn collapses_pipeline_with_helper_when_later_stage_is_unknown() {
        let command = shlex_split_safe("rg --files | nl -ba | foo");
        assert_parsed(
            &command,
            vec![ParsedCommand::Unknown {
                cmd: shlex_join(&command),
            }],
        );
    }

    #[test]
    fn rg_files_with_matches_flags_are_search() {
        assert_parsed(
            &shlex_split_safe("rg -l TODO src"),
            vec![ParsedCommand::Search {
                cmd: "rg -l TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("rg --files-with-matches TODO src"),
            vec![ParsedCommand::Search {
                cmd: "rg --files-with-matches TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("rg -L TODO src"),
            vec![ParsedCommand::Search {
                cmd: "rg -L TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("rg --files-without-match TODO src"),
            vec![ParsedCommand::Search {
                cmd: "rg --files-without-match TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("rga -l TODO src"),
            vec![ParsedCommand::Search {
                cmd: "rga -l TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
    }

    #[test]
    fn supports_cat() {
        let inner = "cat webview/README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("webview/README.md"),
            }],
        );
    }

    #[test]
    fn zsh_lc_supports_cat() {
        let inner = "cat README.md";
        assert_parsed(
            &vec_str(&["zsh", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }],
        );
    }

    #[test]
    fn supports_bat() {
        let inner = "bat --theme TwoDark README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }],
        );
    }

    #[test]
    fn supports_batcat() {
        let inner = "batcat README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }],
        );
    }

    #[test]
    fn supports_less() {
        let inner = "less -p TODO README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }],
        );
    }

    #[test]
    fn supports_more() {
        let inner = "more README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }],
        );
    }

    #[test]
    fn cd_then_cat_is_single_read() {
        assert_parsed(
            &shlex_split_safe("cd foo && cat foo.txt"),
            vec![ParsedCommand::Read {
                cmd: "cat foo.txt".to_string(),
                name: "foo.txt".to_string(),
                path: PathBuf::from("foo/foo.txt"),
            }],
        );
    }

    #[test]
    fn cd_with_double_dash_then_cat_is_read() {
        assert_parsed(
            &shlex_split_safe("cd -- -weird && cat foo.txt"),
            vec![ParsedCommand::Read {
                cmd: "cat foo.txt".to_string(),
                name: "foo.txt".to_string(),
                path: PathBuf::from("-weird/foo.txt"),
            }],
        );
    }

    #[test]
    fn cd_with_multiple_operands_uses_last() {
        assert_parsed(
            &shlex_split_safe("cd dir1 dir2 && cat foo.txt"),
            vec![ParsedCommand::Read {
                cmd: "cat foo.txt".to_string(),
                name: "foo.txt".to_string(),
                path: PathBuf::from("dir2/foo.txt"),
            }],
        );
    }

    #[test]
    fn bash_cd_then_bar_is_same_as_bar() {
        // Ensure a leading `cd` inside bash -lc is dropped when followed by another command.
        assert_parsed(
            &shlex_split_safe("bash -lc 'cd foo && bar'"),
            vec![ParsedCommand::Unknown {
                cmd: "cd foo && bar".to_string(),
            }],
        );
    }

    #[test]
    fn bash_cd_then_cat_is_read() {
        assert_parsed(
            &shlex_split_safe("bash -lc 'cd foo && cat foo.txt'"),
            vec![ParsedCommand::Read {
                cmd: "cat foo.txt".to_string(),
                name: "foo.txt".to_string(),
                path: PathBuf::from("foo/foo.txt"),
            }],
        );
    }

    #[test]
    fn supports_ls_with_pipe() {
        let inner = "ls -la | sed -n '1,120p'";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::ListFiles {
                cmd: "ls -la".to_string(),
                path: None,
            }],
        );
    }

    #[test]
    fn supports_eza_exa_tree_du() {
        assert_parsed(
            &shlex_split_safe("eza --color=always src"),
            vec![ParsedCommand::ListFiles {
                cmd: "eza '--color=always' src".to_string(),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("exa -I target ."),
            vec![ParsedCommand::ListFiles {
                cmd: "exa -I target .".to_string(),
                path: Some(".".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("tree -L 2 src"),
            vec![ParsedCommand::ListFiles {
                cmd: "tree -L 2 src".to_string(),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("du -d 2 ."),
            vec![ParsedCommand::ListFiles {
                cmd: "du -d 2 .".to_string(),
                path: Some(".".to_string()),
            }],
        );
    }

    #[test]
    fn supports_head_n() {
        let inner = "head -n 50 Cargo.toml";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "Cargo.toml".to_string(),
                path: PathBuf::from("Cargo.toml"),
            }],
        );
    }

    #[test]
    fn supports_head_file_only() {
        let inner = "head Cargo.toml";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "Cargo.toml".to_string(),
                path: PathBuf::from("Cargo.toml"),
            }],
        );
    }

    #[test]
    fn supports_cat_sed_n() {
        let inner = "cat tui/Cargo.toml | sed -n '1,200p'";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "Cargo.toml".to_string(),
                path: PathBuf::from("tui/Cargo.toml"),
            }],
        );
    }

    #[test]
    fn supports_tail_n_plus() {
        let inner = "tail -n +522 README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }],
        );
    }

    #[test]
    fn supports_tail_n_last_lines() {
        let inner = "tail -n 30 README.md";
        let out = parse_command(&vec_str(&["bash", "-lc", inner]));
        assert_eq!(
            out,
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }]
        );
    }

    #[test]
    fn supports_tail_file_only() {
        let inner = "tail README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }],
        );
    }

    #[test]
    fn supports_npm_run_build_is_unknown() {
        assert_parsed(
            &vec_str(&["npm", "run", "build"]),
            vec![ParsedCommand::Unknown {
                cmd: "npm run build".to_string(),
            }],
        );
    }

    #[test]
    fn supports_grep_recursive_current_dir() {
        assert_parsed(
            &vec_str(&["grep", "-R", "CHAOS_SANDBOX_ENV_VAR", "-n", "."]),
            vec![ParsedCommand::Search {
                cmd: "grep -R CHAOS_SANDBOX_ENV_VAR -n .".to_string(),
                query: Some("CHAOS_SANDBOX_ENV_VAR".to_string()),
                path: Some(".".to_string()),
            }],
        );
    }

    #[test]
    fn supports_grep_recursive_specific_file() {
        assert_parsed(
            &vec_str(&[
                "grep",
                "-R",
                "CHAOS_SANDBOX_ENV_VAR",
                "-n",
                "core/src/spawn.rs",
            ]),
            vec![ParsedCommand::Search {
                cmd: "grep -R CHAOS_SANDBOX_ENV_VAR -n core/src/spawn.rs".to_string(),
                query: Some("CHAOS_SANDBOX_ENV_VAR".to_string()),
                path: Some("spawn.rs".to_string()),
            }],
        );
    }

    #[test]
    fn supports_egrep_and_fgrep() {
        assert_parsed(
            &shlex_split_safe("egrep -R TODO src"),
            vec![ParsedCommand::Search {
                cmd: "egrep -R TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("fgrep -l TODO src"),
            vec![ParsedCommand::Search {
                cmd: "fgrep -l TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
    }

    #[test]
    fn grep_files_with_matches_flags_are_search() {
        assert_parsed(
            &shlex_split_safe("grep -l TODO src"),
            vec![ParsedCommand::Search {
                cmd: "grep -l TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("grep --files-with-matches TODO src"),
            vec![ParsedCommand::Search {
                cmd: "grep --files-with-matches TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("grep -L TODO src"),
            vec![ParsedCommand::Search {
                cmd: "grep -L TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("grep --files-without-match TODO src"),
            vec![ParsedCommand::Search {
                cmd: "grep --files-without-match TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
    }

    #[test]
    fn supports_grep_query_with_slashes_not_shortened() {
        // Query strings may contain slashes and should not be shortened to the basename.
        // Previously, grep queries were passed through short_display_path, which is incorrect.
        assert_parsed(
            &shlex_split_safe("grep -R src/main.rs -n ."),
            vec![ParsedCommand::Search {
                cmd: "grep -R src/main.rs -n .".to_string(),
                query: Some("src/main.rs".to_string()),
                path: Some(".".to_string()),
            }],
        );
    }

    #[test]
    fn supports_grep_weird_backtick_in_query() {
        assert_parsed(
            &shlex_split_safe("grep -R COD`EX_SANDBOX -n"),
            vec![ParsedCommand::Search {
                cmd: "grep -R 'COD`EX_SANDBOX' -n".to_string(),
                query: Some("COD`EX_SANDBOX".to_string()),
                path: None,
            }],
        );
    }

    #[test]
    fn supports_cd_and_rg_files() {
        assert_parsed(
            &shlex_split_safe("cd chaos && rg --files"),
            vec![ParsedCommand::ListFiles {
                cmd: "rg --files".to_string(),
                path: None,
            }],
        );
    }

    #[test]
    fn supports_single_string_script_with_cd_and_pipe() {
        let inner = r#"cd /Users/pakrym/code/chaos && rg -n "chaos_api" chaos -S | head -n 50"#;
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Search {
                cmd: "rg -n chaos_api chaos -S".to_string(),
                query: Some("chaos_api".to_string()),
                path: Some("chaos".to_string()),
            }],
        );
    }

    #[test]
    fn supports_python_walks_files() {
        let inner = r#"python -c "import os; print(os.listdir('.'))""#;
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::ListFiles {
                cmd: shlex_join(&shlex_split_safe(inner)),
                path: None,
            }],
        );
    }

    #[test]
    fn supports_python3_walks_files() {
        let inner = r#"python3 -c "import glob; print(glob.glob('*.rs'))""#;
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::ListFiles {
                cmd: shlex_join(&shlex_split_safe(inner)),
                path: None,
            }],
        );
    }

    #[test]
    fn python_without_file_walk_is_unknown() {
        let inner = r#"python -c "print('hello')""#;
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Unknown {
                cmd: shlex_join(&shlex_split_safe(inner)),
            }],
        );
    }

    // ---- is_small_formatting_command unit tests ----
    #[test]
    fn small_formatting_always_true_commands() {
        for cmd in ["wc", "tr", "cut", "sort", "uniq", "xargs", "tee", "column"] {
            assert!(is_small_formatting_command(&shlex_split_safe(cmd)));
            assert!(is_small_formatting_command(&shlex_split_safe(&format!(
                "{cmd} -x"
            ))));
        }
    }

    #[test]
    fn awk_behavior() {
        assert!(is_small_formatting_command(&shlex_split_safe(
            "awk '{print $1}'"
        )));
        assert!(!is_small_formatting_command(&shlex_split_safe(
            "awk '{print $1}' Cargo.toml"
        )));
        assert!(!is_small_formatting_command(&shlex_split_safe(
            "awk -f script.awk Cargo.toml"
        )));
    }

    #[test]
    fn head_behavior() {
        // No args -> small formatting
        assert!(is_small_formatting_command(&vec_str(&["head"])));
        // Numeric count only -> formatting
        assert!(is_small_formatting_command(&shlex_split_safe("head -n 40")));
        // With explicit file -> not small formatting
        assert!(!is_small_formatting_command(&shlex_split_safe(
            "head -n 40 file.txt"
        )));
        // File only (no count) -> not formatting
        assert!(!is_small_formatting_command(&vec_str(&[
            "head", "file.txt"
        ])));
    }

    #[test]
    fn tail_behavior() {
        // No args -> small formatting
        assert!(is_small_formatting_command(&vec_str(&["tail"])));
        // Numeric with plus offset -> formatting
        assert!(is_small_formatting_command(&shlex_split_safe(
            "tail -n +10"
        )));
        assert!(!is_small_formatting_command(&shlex_split_safe(
            "tail -n +10 file.txt"
        )));
        // Numeric count -> formatting
        assert!(is_small_formatting_command(&shlex_split_safe("tail -n 30")));
        assert!(!is_small_formatting_command(&shlex_split_safe(
            "tail -n 30 file.txt"
        )));
        // Byte count -> formatting
        assert!(is_small_formatting_command(&shlex_split_safe("tail -c 30")));
        assert!(is_small_formatting_command(&shlex_split_safe(
            "tail -c +10"
        )));
        // File only (no count) -> not formatting
        assert!(!is_small_formatting_command(&vec_str(&[
            "tail", "file.txt"
        ])));
    }

    #[test]
    fn sed_behavior() {
        // Plain sed -> small formatting
        assert!(is_small_formatting_command(&vec_str(&["sed"])));
        // sed -n <range> (no file) -> still small formatting
        assert!(is_small_formatting_command(&vec_str(&["sed", "-n", "10p"])));
        // Valid range with file -> not small formatting
        assert!(!is_small_formatting_command(&shlex_split_safe(
            "sed -n 10p file.txt"
        )));
        assert!(!is_small_formatting_command(&shlex_split_safe(
            "sed -n -e 10p file.txt"
        )));
        assert!(!is_small_formatting_command(&shlex_split_safe(
            "sed -n 10p -- file.txt"
        )));
        assert!(!is_small_formatting_command(&shlex_split_safe(
            "sed -n 1,200p file.txt"
        )));
        // Invalid ranges with file -> small formatting
        assert!(is_small_formatting_command(&shlex_split_safe(
            "sed -n p file.txt"
        )));
        assert!(is_small_formatting_command(&shlex_split_safe(
            "sed -n +10p file.txt"
        )));
    }

    #[test]
    fn empty_tokens_is_not_small() {
        let empty: Vec<String> = Vec::new();
        assert!(!is_small_formatting_command(&empty));
    }

    #[test]
    fn supports_nl_then_sed_reading() {
        let inner = "nl -ba core/src/parse_command.rs | sed -n '1200,1720p'";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "parse_command.rs".to_string(),
                path: PathBuf::from("core/src/parse_command.rs"),
            }],
        );
    }

    #[test]
    fn supports_sed_n() {
        let inner = "sed -n '2000,2200p' tui/src/history_cell.rs";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "history_cell.rs".to_string(),
                path: PathBuf::from("tui/src/history_cell.rs"),
            }],
        );
    }

    #[test]
    fn supports_awk_with_file() {
        let inner = "awk '{print $1}' Cargo.toml";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "Cargo.toml".to_string(),
                path: PathBuf::from("Cargo.toml"),
            }],
        );
    }

    #[test]
    fn filters_out_printf() {
        let inner =
            r#"printf "\n===== ansi-escape/Cargo.toml =====\n"; cat -- ansi-escape/Cargo.toml"#;
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: "cat -- ansi-escape/Cargo.toml".to_string(),
                name: "Cargo.toml".to_string(),
                path: PathBuf::from("ansi-escape/Cargo.toml"),
            }],
        );
    }

    #[test]
    fn drops_yes_in_pipelines() {
        // Inside bash -lc, `yes | rg --files` should focus on the primary command.
        let inner = "yes | rg --files";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::ListFiles {
                cmd: "rg --files".to_string(),
                path: None,
            }],
        );
    }

    #[test]
    fn supports_sed_n_then_nl_as_search() {
        // Ensure `sed -n '<range>' <file> | nl -ba` is summarized as a search for that file.
        let args = shlex_split_safe(
            "sed -n '260,640p' exec/src/event_processor_with_human_output.rs | nl -ba",
        );
        assert_parsed(
            &args,
            vec![ParsedCommand::Read {
                cmd: "sed -n '260,640p' exec/src/event_processor_with_human_output.rs".to_string(),
                name: "event_processor_with_human_output.rs".to_string(),
                path: PathBuf::from("exec/src/event_processor_with_human_output.rs"),
            }],
        );
    }

    #[test]
    fn preserves_rg_with_spaces() {
        assert_parsed(
            &shlex_split_safe("yes | rg -n 'foo bar' -S"),
            vec![ParsedCommand::Search {
                cmd: "rg -n 'foo bar' -S".to_string(),
                query: Some("foo bar".to_string()),
                path: None,
            }],
        );
    }

    #[test]
    fn ls_with_glob() {
        assert_parsed(
            &shlex_split_safe("ls -I '*.test.js'"),
            vec![ParsedCommand::ListFiles {
                cmd: "ls -I '*.test.js'".to_string(),
                path: None,
            }],
        );
    }

    #[test]
    fn strips_true_in_sequence() {
        // `true` should be dropped from parsed sequences
        assert_parsed(
            &shlex_split_safe("true && rg --files"),
            vec![ParsedCommand::ListFiles {
                cmd: "rg --files".to_string(),
                path: None,
            }],
        );

        assert_parsed(
            &shlex_split_safe("rg --files && true"),
            vec![ParsedCommand::ListFiles {
                cmd: "rg --files".to_string(),
                path: None,
            }],
        );
    }

    #[test]
    fn strips_true_inside_bash_lc() {
        let inner = "true && rg --files";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::ListFiles {
                cmd: "rg --files".to_string(),
                path: None,
            }],
        );

        let inner2 = "rg --files || true";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner2]),
            vec![ParsedCommand::ListFiles {
                cmd: "rg --files".to_string(),
                path: None,
            }],
        );
    }

    #[test]
    fn shorten_path_on_windows() {
        assert_parsed(
            &shlex_split_safe(r#"cat "pkg\src\main.rs""#),
            vec![ParsedCommand::Read {
                cmd: r#"cat "pkg\\src\\main.rs""#.to_string(),
                name: "main.rs".to_string(),
                path: PathBuf::from(r#"pkg\src\main.rs"#),
            }],
        );
    }

    #[test]
    fn head_with_no_space() {
        assert_parsed(
            &shlex_split_safe("bash -lc 'head -n50 Cargo.toml'"),
            vec![ParsedCommand::Read {
                cmd: "head -n50 Cargo.toml".to_string(),
                name: "Cargo.toml".to_string(),
                path: PathBuf::from("Cargo.toml"),
            }],
        );
    }

    #[test]
    fn bash_dash_c_pipeline_parsing() {
        // Ensure -c is handled similarly to -lc by shell parsing
        let inner = "rg --files | head -n 1";
        assert_parsed(
            &vec_str(&["bash", "-c", inner]),
            vec![ParsedCommand::ListFiles {
                cmd: "rg --files".to_string(),
                path: None,
            }],
        );
    }

    #[test]
    fn tail_with_no_space() {
        assert_parsed(
            &shlex_split_safe("bash -lc 'tail -n+10 README.md'"),
            vec![ParsedCommand::Read {
                cmd: "tail -n+10 README.md".to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }],
        );
    }

    #[test]
    fn grep_with_query_and_path() {
        assert_parsed(
            &shlex_split_safe("grep -R TODO src"),
            vec![ParsedCommand::Search {
                cmd: "grep -R TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
    }

    #[test]
    fn supports_ag_ack_pt_rga() {
        assert_parsed(
            &shlex_split_safe("ag TODO src"),
            vec![ParsedCommand::Search {
                cmd: "ag TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("ack TODO src"),
            vec![ParsedCommand::Search {
                cmd: "ack TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("pt TODO src"),
            vec![ParsedCommand::Search {
                cmd: "pt TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("rga TODO src"),
            vec![ParsedCommand::Search {
                cmd: "rga TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
    }

    #[test]
    fn ag_ack_pt_files_with_matches_flags_are_search() {
        assert_parsed(
            &shlex_split_safe("ag -l TODO src"),
            vec![ParsedCommand::Search {
                cmd: "ag -l TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("ack -l TODO src"),
            vec![ParsedCommand::Search {
                cmd: "ack -l TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("pt -l TODO src"),
            vec![ParsedCommand::Search {
                cmd: "pt -l TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
    }

    #[test]
    fn rg_with_equals_style_flags() {
        assert_parsed(
            &shlex_split_safe("rg --colors=never -n foo src"),
            vec![ParsedCommand::Search {
                cmd: "rg '--colors=never' -n foo src".to_string(),
                query: Some("foo".to_string()),
                path: Some("src".to_string()),
            }],
        );
    }

    #[test]
    fn cat_with_double_dash_and_sed_ranges() {
        // cat -- <file> should be treated as a read of that file
        assert_parsed(
            &shlex_split_safe("cat -- ./-strange-file-name"),
            vec![ParsedCommand::Read {
                cmd: "cat -- ./-strange-file-name".to_string(),
                name: "-strange-file-name".to_string(),
                path: PathBuf::from("./-strange-file-name"),
            }],
        );

        // sed -n <range> <file> should be treated as a read of <file>
        assert_parsed(
            &shlex_split_safe("sed -n '12,20p' Cargo.toml"),
            vec![ParsedCommand::Read {
                cmd: "sed -n '12,20p' Cargo.toml".to_string(),
                name: "Cargo.toml".to_string(),
                path: PathBuf::from("Cargo.toml"),
            }],
        );
    }

    #[test]
    fn drop_trailing_nl_in_pipeline() {
        // When an `nl` stage has only flags, it should be dropped from the summary
        assert_parsed(
            &shlex_split_safe("rg --files | nl -ba"),
            vec![ParsedCommand::ListFiles {
                cmd: "rg --files".to_string(),
                path: None,
            }],
        );
    }

    #[test]
    fn ls_with_time_style_and_path() {
        assert_parsed(
            &shlex_split_safe("ls --time-style=long-iso ./dist"),
            vec![ParsedCommand::ListFiles {
                cmd: "ls '--time-style=long-iso' ./dist".to_string(),
                // short_display_path drops "dist" and shows "." as the last useful segment
                path: Some(".".to_string()),
            }],
        );
    }

    #[test]
    fn fd_file_finder_variants() {
        assert_parsed(
            &shlex_split_safe("fd -t f src/"),
            vec![ParsedCommand::ListFiles {
                cmd: "fd -t f src/".to_string(),
                path: Some("src".to_string()),
            }],
        );

        // fd with query and path should capture both
        assert_parsed(
            &shlex_split_safe("fd main src"),
            vec![ParsedCommand::Search {
                cmd: "fd main src".to_string(),
                query: Some("main".to_string()),
                path: Some("src".to_string()),
            }],
        );
    }

    #[test]
    fn find_basic_name_filter() {
        assert_parsed(
            &shlex_split_safe("find . -name '*.rs'"),
            vec![ParsedCommand::Search {
                cmd: "find . -name '*.rs'".to_string(),
                query: Some("*.rs".to_string()),
                path: Some(".".to_string()),
            }],
        );
    }

    #[test]
    fn find_type_only_path() {
        assert_parsed(
            &shlex_split_safe("find src -type f"),
            vec![ParsedCommand::ListFiles {
                cmd: "find src -type f".to_string(),
                path: Some("src".to_string()),
            }],
        );
    }

    #[test]
    fn bin_bash_lc_sed() {
        assert_parsed(
            &shlex_split_safe("/bin/bash -lc 'sed -n '1,10p' Cargo.toml'"),
            vec![ParsedCommand::Read {
                cmd: "sed -n '1,10p' Cargo.toml".to_string(),
                name: "Cargo.toml".to_string(),
                path: PathBuf::from("Cargo.toml"),
            }],
        );
    }
    #[test]
    fn bin_zsh_lc_sed() {
        assert_parsed(
            &shlex_split_safe("/bin/zsh -lc 'sed -n '1,10p' Cargo.toml'"),
            vec![ParsedCommand::Read {
                cmd: "sed -n '1,10p' Cargo.toml".to_string(),
                name: "Cargo.toml".to_string(),
                path: PathBuf::from("Cargo.toml"),
            }],
        );
    }
}
