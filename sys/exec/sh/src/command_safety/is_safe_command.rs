use crate::bash::parse_shell_lc_plain_commands;
use crate::command_safety::is_dangerous_command::executable_name_lookup_key;
// Find the first matching git subcommand, skipping known global options that
// may appear before it (e.g., `-C`, `-c`, `--git-dir`).
// Implemented in `is_dangerous_command` and shared here.
use crate::command_safety::is_dangerous_command::find_git_subcommand;
pub fn is_known_safe_command(command: &[String]) -> bool {
    let command: Vec<String> = command
        .iter()
        .map(|s| {
            if s == "zsh" {
                "bash".to_string()
            } else {
                s.clone()
            }
        })
        .collect();

    if is_safe_to_call_with_exec(&command) {
        return true;
    }

    // Support `bash -lc "..."` where the script consists solely of one or
    // more "plain" commands (only bare words / quoted strings) combined with
    // a conservative allow‑list of shell operators that themselves do not
    // introduce side effects ( "&&", "||", ";", and "|" ). If every
    // individual command in the script is itself a known‑safe command, then
    // the composite expression is considered safe.
    if let Some(all_commands) = parse_shell_lc_plain_commands(&command)
        && !all_commands.is_empty()
        && all_commands
            .iter()
            .all(|cmd| is_safe_to_call_with_exec(cmd))
    {
        return true;
    }
    false
}

fn is_safe_to_call_with_exec(command: &[String]) -> bool {
    let Some(cmd0) = command.first().map(String::as_str) else {
        return false;
    };

    match executable_name_lookup_key(cmd0).as_deref() {
        Some(cmd) if cfg!(target_os = "linux") && matches!(cmd, "numfmt" | "tac") => true,

        #[rustfmt::skip]
        Some(
            "cat" |
            "cd" |
            "cut" |
            "echo" |
            "expr" |
            "false" |
            "grep" |
            "head" |
            "id" |
            "ls" |
            "nl" |
            "paste" |
            "pwd" |
            "rev" |
            "seq" |
            "stat" |
            "tail" |
            "tr" |
            "true" |
            "uname" |
            "uniq" |
            "wc" |
            "which" |
            "whoami") => {
            true
        },

        Some("base64") => {
            const UNSAFE_BASE64_OPTIONS: &[&str] = &["-o", "--output"];

            !command.iter().skip(1).any(|arg| {
                UNSAFE_BASE64_OPTIONS.contains(&arg.as_str())
                    || arg.starts_with("--output=")
                    || (arg.starts_with("-o") && arg != "-o")
            })
        }

        Some("find") => {
            // Certain options to `find` can delete files, write to files, or
            // execute arbitrary commands, so we cannot auto-approve the
            // invocation of `find` in such cases.
            #[rustfmt::skip]
            const UNSAFE_FIND_OPTIONS: &[&str] = &[
                // Options that can execute arbitrary commands.
                "-exec", "-execdir", "-ok", "-okdir",
                // Option that deletes matching files.
                "-delete",
                // Options that write pathnames to a file.
                "-fls", "-fprint", "-fprint0", "-fprintf",
            ];

            !command
                .iter()
                .any(|arg| UNSAFE_FIND_OPTIONS.contains(&arg.as_str()))
        }

        // Ripgrep
        Some("rg") => {
            const UNSAFE_RIPGREP_OPTIONS_WITH_ARGS: &[&str] = &[
                // Takes an arbitrary command that is executed for each match.
                "--pre",
                // Takes a command that can be used to obtain the local hostname.
                "--hostname-bin",
            ];
            const UNSAFE_RIPGREP_OPTIONS_WITHOUT_ARGS: &[&str] = &[
                // Calls out to other decompression tools, so do not auto-approve
                // out of an abundance of caution.
                "--search-zip",
                "-z",
            ];

            !command.iter().any(|arg| {
                UNSAFE_RIPGREP_OPTIONS_WITHOUT_ARGS.contains(&arg.as_str())
                    || UNSAFE_RIPGREP_OPTIONS_WITH_ARGS
                        .iter()
                        .any(|&opt| arg == opt || arg.starts_with(&format!("{opt}=")))
            })
        }

        // Git
        Some("git") => {
            // Global config overrides like `-c core.pager=...` can force git
            // to execute arbitrary external commands. With no sandboxing, we
            // should always prompt in those cases.
            if git_has_config_override_global_option(command) {
                return false;
            }

            let Some((subcommand_idx, subcommand)) =
                find_git_subcommand(command, &["status", "log", "diff", "show", "branch"])
            else {
                return false;
            };

            let subcommand_args = &command[subcommand_idx + 1..];

            match subcommand {
                "status" | "log" | "diff" | "show" => {
                    git_subcommand_args_are_read_only(subcommand_args)
                }
                "branch" => {
                    git_subcommand_args_are_read_only(subcommand_args)
                        && git_branch_is_read_only(subcommand_args)
                }
                other => {
                    debug_assert!(false, "unexpected git subcommand from matcher: {other}");
                    false
                }
            }
        }

        // Special-case `sed -n {N|M,N}p`
        Some("sed")
            if {
                command.len() <= 4
                    && command.get(1).map(String::as_str) == Some("-n")
                    && super::is_valid_sed_n_arg(command.get(2).map(String::as_str))
            } =>
        {
            true
        }

        // ── anything else ─────────────────────────────────────────────────
        _ => false,
    }
}

// Treat `git branch` as safe only when the arguments clearly indicate
// a read-only query, not a branch mutation (create/rename/delete).
fn git_branch_is_read_only(branch_args: &[String]) -> bool {
    if branch_args.is_empty() {
        // `git branch` with no additional args lists branches.
        return true;
    }

    let mut saw_read_only_flag = false;
    for arg in branch_args.iter().map(String::as_str) {
        match arg {
            "--list" | "-l" | "--show-current" | "-a" | "--all" | "-r" | "--remotes" | "-v"
            | "-vv" | "--verbose" => {
                saw_read_only_flag = true;
            }
            _ if arg.starts_with("--format=") => {
                saw_read_only_flag = true;
            }
            _ => {
                // Any other flag or positional argument may create, rename, or delete branches.
                return false;
            }
        }
    }

    saw_read_only_flag
}

fn git_has_config_override_global_option(command: &[String]) -> bool {
    command.iter().map(String::as_str).any(|arg| {
        matches!(arg, "-c" | "--config-env")
            || (arg.starts_with("-c") && arg.len() > 2)
            || arg.starts_with("--config-env=")
    })
}

fn git_subcommand_args_are_read_only(args: &[String]) -> bool {
    // Flags that can write to disk or execute external tools should never be
    // auto-approved on an unsandboxed machine.
    const UNSAFE_GIT_FLAGS: &[&str] = &[
        "--output",
        "--ext-diff",
        "--textconv",
        "--exec",
        "--paginate",
    ];

    !args.iter().map(String::as_str).any(|arg| {
        UNSAFE_GIT_FLAGS.contains(&arg)
            || arg.starts_with("--output=")
            || arg.starts_with("--exec=")
    })
}

// (bash parsing helpers implemented in crate::bash)

#[cfg(test)]
mod tests;
