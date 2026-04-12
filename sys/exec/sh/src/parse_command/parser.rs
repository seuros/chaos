//! Parsing logic: assembles `ParsedCommand` summaries from token sequences.

use super::ast::{
    awk_data_file_operand_inner, first_non_flag_operand, is_pathish, join_paths,
    short_display_path, single_non_flag_operand, skip_flag_values,
};
use super::lexer::{
    contains_connectors, normalize_tokens, shlex_join, split_on_connectors, trim_at_connector,
};
use crate::bash::extract_bash_command;
use crate::bash::try_parse_shell;
use crate::bash::try_parse_word_only_commands_sequence;
use chaos_ipc::parse_command::ParsedCommand;
use shlex::split as shlex_split;
use std::path::PathBuf;

/// Extracts the shell and script from a command, regardless of platform.
pub fn extract_shell_command(command: &[String]) -> Option<(&str, &str)> {
    extract_bash_command(command)
}

pub fn single_unknown_for_command(command: &[String]) -> ParsedCommand {
    if let Some((_, shell_command)) = extract_shell_command(command) {
        ParsedCommand::Unknown {
            cmd: shell_command.to_string(),
        }
    } else {
        ParsedCommand::Unknown {
            cmd: shlex_join(command),
        }
    }
}

pub fn parse_command_impl(command: &[String]) -> Vec<ParsedCommand> {
    if let Some(commands) = parse_shell_lc_commands(command) {
        return commands;
    }

    let normalized = normalize_tokens(command);

    let parts = if contains_connectors(&normalized) {
        split_on_connectors(&normalized)
    } else {
        vec![normalized]
    };

    // Preserve left-to-right execution order for all commands, including bash -c/-lc
    // so summaries reflect the order they will run.

    // Map each pipeline segment to its parsed summary, tracking `cd` to compute paths.
    let mut commands: Vec<ParsedCommand> = Vec::new();
    let mut cwd: Option<String> = None;
    for tokens in &parts {
        if let Some((head, tail)) = tokens.split_first()
            && head == "cd"
        {
            if let Some(dir) = cd_target(tail) {
                cwd = Some(match &cwd {
                    Some(base) => join_paths(base, &dir),
                    None => dir.clone(),
                });
            }
            continue;
        }
        let parsed = summarize_main_tokens(tokens);
        let parsed = match parsed {
            ParsedCommand::Read { cmd, name, path } => {
                if let Some(base) = &cwd {
                    let full = join_paths(base, &path.to_string_lossy());
                    ParsedCommand::Read {
                        cmd,
                        name,
                        path: PathBuf::from(full),
                    }
                } else {
                    ParsedCommand::Read { cmd, name, path }
                }
            }
            other => other,
        };
        commands.push(parsed);
    }

    while let Some(next) = simplify_once(&commands) {
        commands = next;
    }

    commands
}

pub fn simplify_once(commands: &[ParsedCommand]) -> Option<Vec<ParsedCommand>> {
    if commands.len() <= 1 {
        return None;
    }

    // echo ... && ...rest => ...rest
    if let ParsedCommand::Unknown { cmd } = &commands[0]
        && shlex_split(cmd).is_some_and(|t| t.first().map(String::as_str) == Some("echo"))
    {
        return Some(commands[1..].to_vec());
    }

    // cd foo && [any command] => [any command] (keep non-cd when a cd is followed by something)
    if let Some(idx) = commands.iter().position(|pc| match pc {
        ParsedCommand::Unknown { cmd } => {
            shlex_split(cmd).is_some_and(|t| t.first().map(String::as_str) == Some("cd"))
        }
        _ => false,
    }) && commands.len() > idx + 1
    {
        let mut out = Vec::with_capacity(commands.len() - 1);
        out.extend_from_slice(&commands[..idx]);
        out.extend_from_slice(&commands[idx + 1..]);
        return Some(out);
    }

    // cmd || true => cmd
    if let Some(idx) = commands
        .iter()
        .position(|pc| matches!(pc, ParsedCommand::Unknown { cmd } if cmd == "true"))
    {
        let mut out = Vec::with_capacity(commands.len() - 1);
        out.extend_from_slice(&commands[..idx]);
        out.extend_from_slice(&commands[idx + 1..]);
        return Some(out);
    }

    // nl -[any_flags] && ...rest => ...rest
    if let Some(idx) = commands.iter().position(|pc| match pc {
        ParsedCommand::Unknown { cmd } => {
            if let Some(tokens) = shlex_split(cmd) {
                tokens.first().is_some_and(|s| s.as_str() == "nl")
                    && tokens.iter().skip(1).all(|t| t.starts_with('-'))
            } else {
                false
            }
        }
        _ => false,
    }) {
        let mut out = Vec::with_capacity(commands.len() - 1);
        out.extend_from_slice(&commands[..idx]);
        out.extend_from_slice(&commands[idx + 1..]);
        return Some(out);
    }

    None
}

/// Validates that this is a `sed -n 123,123p` command.
fn is_valid_sed_n_arg(arg: Option<&str>) -> bool {
    let s = match arg {
        Some(s) => s,
        None => return false,
    };
    let core = match s.strip_suffix('p') {
        Some(rest) => rest,
        None => return false,
    };
    let parts: Vec<&str> = core.split(',').collect();
    match parts.as_slice() {
        [num] => !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()),
        [a, b] => {
            !a.is_empty()
                && !b.is_empty()
                && a.chars().all(|c| c.is_ascii_digit())
                && b.chars().all(|c| c.is_ascii_digit())
        }
        _ => false,
    }
}

fn sed_read_path(args: &[String]) -> Option<String> {
    let args_no_connector = trim_at_connector(args);
    if !args_no_connector.iter().any(|arg| arg == "-n") {
        return None;
    }
    let mut has_range_script = false;
    let mut i = 0;
    while i < args_no_connector.len() {
        let arg = &args_no_connector[i];
        if matches!(arg.as_str(), "-e" | "--expression") {
            if is_valid_sed_n_arg(args_no_connector.get(i + 1).map(String::as_str)) {
                has_range_script = true;
            }
            i += 2;
            continue;
        }
        if matches!(arg.as_str(), "-f" | "--file") {
            i += 2;
            continue;
        }
        i += 1;
    }
    if !has_range_script {
        has_range_script = args_no_connector
            .iter()
            .any(|arg| !arg.starts_with('-') && is_valid_sed_n_arg(Some(arg)));
    }
    if !has_range_script {
        return None;
    }
    let candidates = skip_flag_values(&args_no_connector, &["-e", "-f", "--expression", "--file"]);
    let non_flags: Vec<String> = candidates
        .into_iter()
        .filter(|arg| !arg.starts_with('-'))
        .cloned()
        .collect();
    match non_flags.as_slice() {
        [] => None,
        [first, rest @ ..] if is_valid_sed_n_arg(Some(first)) => rest.first().cloned(),
        [first, ..] => Some(first.clone()),
    }
}

fn parse_grep_like(main_cmd: &[String], args: &[String]) -> ParsedCommand {
    let args_no_connector = trim_at_connector(args);
    let mut operands = Vec::new();
    let mut pattern: Option<String> = None;
    let mut after_double_dash = false;
    let mut iter = args_no_connector.iter().peekable();
    while let Some(arg) = iter.next() {
        if after_double_dash {
            operands.push(arg);
            continue;
        }
        if arg == "--" {
            after_double_dash = true;
            continue;
        }
        match arg.as_str() {
            "-e" | "--regexp" => {
                if let Some(pat) = iter.next()
                    && pattern.is_none()
                {
                    pattern = Some(pat.clone());
                }
                continue;
            }
            "-f" | "--file" => {
                if let Some(pat_file) = iter.next()
                    && pattern.is_none()
                {
                    pattern = Some(pat_file.clone());
                }
                continue;
            }
            "-m" | "--max-count" | "-C" | "--context" | "-A" | "--after-context" | "-B"
            | "--before-context" => {
                iter.next();
                continue;
            }
            _ => {}
        }
        if arg.starts_with('-') {
            continue;
        }
        operands.push(arg);
    }
    // Do not shorten the query: grep patterns may legitimately contain slashes
    // and should be preserved verbatim. Only paths should be shortened.
    let has_pattern = pattern.is_some();
    let query = pattern.or_else(|| operands.first().cloned().map(String::from));
    let path_index = if has_pattern { 0 } else { 1 };
    let path = operands.get(path_index).map(|s| short_display_path(s));
    ParsedCommand::Search {
        cmd: shlex_join(main_cmd),
        query,
        path,
    }
}

fn python_walks_files(args: &[String]) -> bool {
    let args_no_connector = trim_at_connector(args);
    let mut iter = args_no_connector.iter();
    while let Some(arg) = iter.next() {
        if arg == "-c"
            && let Some(script) = iter.next()
        {
            return script.contains("os.walk")
                || script.contains("os.listdir")
                || script.contains("os.scandir")
                || script.contains("glob.glob")
                || script.contains("glob.iglob")
                || script.contains("pathlib.Path")
                || script.contains(".rglob(");
        }
    }
    false
}

fn is_python_command(cmd: &str) -> bool {
    cmd == "python"
        || cmd == "python2"
        || cmd == "python3"
        || cmd.starts_with("python2.")
        || cmd.starts_with("python3.")
}

fn cd_target(args: &[String]) -> Option<String> {
    if args.is_empty() {
        return None;
    }
    let mut i = 0;
    let mut target: Option<String> = None;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            return args.get(i + 1).cloned();
        }
        if matches!(arg.as_str(), "-L" | "-P") {
            i += 1;
            continue;
        }
        if arg.starts_with('-') {
            i += 1;
            continue;
        }
        target = Some(arg.clone());
        i += 1;
    }
    target
}

fn parse_fd_query_and_path(tail: &[String]) -> (Option<String>, Option<String>) {
    let args_no_connector = trim_at_connector(tail);
    // fd has several flags that take values (e.g., -t/--type, -e/--extension).
    // Skip those values when extracting positional operands.
    let candidates = skip_flag_values(
        &args_no_connector,
        &[
            "-t",
            "--type",
            "-e",
            "--extension",
            "-E",
            "--exclude",
            "--search-path",
        ],
    );
    let non_flags: Vec<&String> = candidates
        .into_iter()
        .filter(|p| !p.starts_with('-'))
        .collect();
    match non_flags.as_slice() {
        [one] => {
            if is_pathish(one) {
                (None, Some(short_display_path(one)))
            } else {
                (Some((*one).clone()), None)
            }
        }
        [q, p, ..] => (Some((*q).clone()), Some(short_display_path(p))),
        _ => (None, None),
    }
}

fn parse_find_query_and_path(tail: &[String]) -> (Option<String>, Option<String>) {
    let args_no_connector = trim_at_connector(tail);
    // First positional argument (excluding common unary operators) is the root path
    let mut path: Option<String> = None;
    for a in &args_no_connector {
        if !a.starts_with('-') && *a != "!" && *a != "(" && *a != ")" {
            path = Some(short_display_path(a));
            break;
        }
    }
    // Extract a common name/path/regex pattern if present
    let mut query: Option<String> = None;
    let mut i = 0;
    while i < args_no_connector.len() {
        let a = &args_no_connector[i];
        if a == "-name" || a == "-iname" || a == "-path" || a == "-regex" {
            if i + 1 < args_no_connector.len() {
                query = Some(args_no_connector[i + 1].clone());
            }
            break;
        }
        i += 1;
    }
    (query, path)
}

fn parse_shell_lc_commands(original: &[String]) -> Option<Vec<ParsedCommand>> {
    // Only handle bash/zsh here.
    let (_, script) = extract_bash_command(original)?;

    if let Some(tree) = try_parse_shell(script)
        && let Some(all_commands) = try_parse_word_only_commands_sequence(&tree, script)
        && !all_commands.is_empty()
    {
        let script_tokens = shlex_split(script).unwrap_or_else(|| vec![script.to_string()]);
        // Strip small formatting helpers (e.g., head/tail/awk/wc/etc) so we
        // bias toward the primary command when pipelines are present.
        // First, drop obvious small formatting helpers (e.g., wc/awk/etc).
        let had_multiple_commands = all_commands.len() > 1;
        // Commands arrive in source order; drop formatting helpers while preserving it.
        let filtered_commands = drop_small_formatting_commands(all_commands);
        if filtered_commands.is_empty() {
            return Some(vec![ParsedCommand::Unknown {
                cmd: script.to_string(),
            }]);
        }
        // Build parsed commands, tracking `cd` segments to compute effective file paths.
        let mut commands: Vec<ParsedCommand> = Vec::new();
        let mut cwd: Option<String> = None;
        for tokens in filtered_commands.into_iter() {
            if let Some((head, tail)) = tokens.split_first()
                && head == "cd"
            {
                if let Some(dir) = cd_target(tail) {
                    cwd = Some(match &cwd {
                        Some(base) => join_paths(base, &dir),
                        None => dir.clone(),
                    });
                }
                continue;
            }
            let parsed = summarize_main_tokens(&tokens);
            let parsed = match parsed {
                ParsedCommand::Read { cmd, name, path } => {
                    if let Some(base) = &cwd {
                        let full = join_paths(base, &path.to_string_lossy());
                        ParsedCommand::Read {
                            cmd,
                            name,
                            path: PathBuf::from(full),
                        }
                    } else {
                        ParsedCommand::Read { cmd, name, path }
                    }
                }
                other => other,
            };
            commands.push(parsed);
        }

        if commands.len() > 1 {
            commands.retain(|pc| !matches!(pc, ParsedCommand::Unknown { cmd } if cmd == "true"));
            // Apply the same simplifications used for non-bash parsing, e.g., drop leading `cd`.
            while let Some(next) = simplify_once(&commands) {
                commands = next;
            }
        }
        if commands.len() == 1 {
            // If we reduced to a single command, attribute the full original script
            // for clearer UX in file-reading and listing scenarios, or when there were
            // no connectors in the original script. For pipeline commands (e.g.
            // `rg --files | sed -n`), keep only the primary command.
            let had_connectors = had_multiple_commands
                || script_tokens
                    .iter()
                    .any(|t| t == "|" || t == "&&" || t == "||" || t == ";");
            commands = commands
                .into_iter()
                .map(|pc| match pc {
                    ParsedCommand::Read { name, cmd, path } => {
                        if had_connectors {
                            let has_pipe = script_tokens.iter().any(|t| t == "|");
                            let has_sed_n = script_tokens.windows(2).any(|w| {
                                w.first().map(String::as_str) == Some("sed")
                                    && w.get(1).map(String::as_str) == Some("-n")
                            });
                            if has_pipe && has_sed_n {
                                ParsedCommand::Read {
                                    cmd: script.to_string(),
                                    name,
                                    path,
                                }
                            } else {
                                ParsedCommand::Read { cmd, name, path }
                            }
                        } else {
                            ParsedCommand::Read {
                                cmd: shlex_join(&script_tokens),
                                name,
                                path,
                            }
                        }
                    }
                    ParsedCommand::ListFiles { path, cmd, .. } => {
                        if had_connectors {
                            ParsedCommand::ListFiles { cmd, path }
                        } else {
                            ParsedCommand::ListFiles {
                                cmd: shlex_join(&script_tokens),
                                path,
                            }
                        }
                    }
                    ParsedCommand::Search {
                        query, path, cmd, ..
                    } => {
                        if had_connectors {
                            ParsedCommand::Search { cmd, query, path }
                        } else {
                            ParsedCommand::Search {
                                cmd: shlex_join(&script_tokens),
                                query,
                                path,
                            }
                        }
                    }
                    other => other,
                })
                .collect();
        }
        return Some(commands);
    }
    Some(vec![ParsedCommand::Unknown {
        cmd: script.to_string(),
    }])
}

/// Return true if this looks like a small formatting helper in a pipeline.
/// Examples: `head -n 40`, `tail -n +10`, `wc -l`, `awk ...`, `cut ...`, `tr ...`.
/// We try to keep variants that clearly include a file path (e.g. `tail -n 30 file`).
pub fn is_small_formatting_command(tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    let cmd = tokens[0].as_str();
    match cmd {
        // Always formatting; typically used in pipes.
        // `nl` is special-cased below to allow `nl <file>` to be treated as a read command.
        "wc" | "tr" | "cut" | "sort" | "uniq" | "tee" | "column" | "yes" | "printf" => true,
        "xargs" => !is_mutating_xargs_command(tokens),
        "awk" => awk_data_file_operand_inner(&tokens[1..]).is_none(),
        "head" => {
            // Treat as formatting when no explicit file operand is present.
            // Common forms: `head -n 40`, `head -c 100`.
            // Keep cases like `head -n 40 file`.
            match tokens {
                // `head`
                [_] => true,
                // `head <file>` or `head -n50`/`head -c100`
                [_, arg] => arg.starts_with('-'),
                // `head -n 40` / `head -c 100` (no file operand)
                [_, flag, count]
                    if (flag == "-n" || flag == "-c")
                        && count.chars().all(|c| c.is_ascii_digit()) =>
                {
                    true
                }
                _ => false,
            }
        }
        "tail" => {
            // Treat as formatting when no explicit file operand is present.
            // Common forms: `tail -n +10`, `tail -n 30`, `tail -c 100`.
            // Keep cases like `tail -n 30 file`.
            match tokens {
                // `tail`
                [_] => true,
                // `tail <file>` or `tail -n30`/`tail -n+10`
                [_, arg] => arg.starts_with('-'),
                // `tail -n 30` / `tail -n +10` (no file operand)
                [_, flag, count]
                    if flag == "-n"
                        && (count.chars().all(|c| c.is_ascii_digit())
                            || (count.starts_with('+')
                                && count[1..].chars().all(|c| c.is_ascii_digit()))) =>
                {
                    true
                }
                // `tail -c 100` / `tail -c +10` (no file operand)
                [_, flag, count]
                    if flag == "-c"
                        && (count.chars().all(|c| c.is_ascii_digit())
                            || (count.starts_with('+')
                                && count[1..].chars().all(|c| c.is_ascii_digit()))) =>
                {
                    true
                }
                _ => false,
            }
        }
        "sed" => {
            // Keep `sed -n <range> file` (treated as a file read elsewhere);
            // otherwise consider it a formatting helper in a pipeline.
            sed_read_path(&tokens[1..]).is_none()
        }
        _ => false,
    }
}

fn is_mutating_xargs_command(tokens: &[String]) -> bool {
    xargs_subcommand(tokens).is_some_and(xargs_is_mutating_subcommand)
}

fn xargs_subcommand(tokens: &[String]) -> Option<&[String]> {
    if tokens.first().map(String::as_str) != Some("xargs") {
        return None;
    }
    let mut i = 1;
    while i < tokens.len() {
        let token = &tokens[i];
        if token == "--" {
            return tokens.get(i + 1..).filter(|rest| !rest.is_empty());
        }
        if !token.starts_with('-') {
            return tokens.get(i..).filter(|rest| !rest.is_empty());
        }
        let takes_value = matches!(
            token.as_str(),
            "-E" | "-e" | "-I" | "-L" | "-n" | "-P" | "-s"
        );
        if takes_value && token.len() == 2 {
            i += 2;
        } else {
            i += 1;
        }
    }
    None
}

fn xargs_is_mutating_subcommand(tokens: &[String]) -> bool {
    let Some((head, tail)) = tokens.split_first() else {
        return false;
    };
    match head.as_str() {
        "perl" | "ruby" => xargs_has_in_place_flag(tail),
        "sed" => xargs_has_in_place_flag(tail) || tail.iter().any(|token| token == "--in-place"),
        "rg" => tail.iter().any(|token| token == "--replace"),
        _ => false,
    }
}

fn xargs_has_in_place_flag(tokens: &[String]) -> bool {
    tokens.iter().any(|token| {
        token == "-i" || token.starts_with("-i") || token == "-pi" || token.starts_with("-pi")
    })
}

fn drop_small_formatting_commands(mut commands: Vec<Vec<String>>) -> Vec<Vec<String>> {
    commands.retain(|tokens| !is_small_formatting_command(tokens));
    commands
}

pub fn summarize_main_tokens(main_cmd: &[String]) -> ParsedCommand {
    match main_cmd.split_first() {
        Some((head, tail)) if matches!(head.as_str(), "ls" | "eza" | "exa") => {
            let flags_with_vals: &[&str] = match head.as_str() {
                "ls" => &[
                    "-I",
                    "-w",
                    "--block-size",
                    "--format",
                    "--time-style",
                    "--color",
                    "--quoting-style",
                ],
                "eza" | "exa" => &[
                    "-I",
                    "--ignore-glob",
                    "--color",
                    "--sort",
                    "--time-style",
                    "--time",
                ],
                _ => &[],
            };
            let path =
                first_non_flag_operand(tail, flags_with_vals).map(|p| short_display_path(&p));
            ParsedCommand::ListFiles {
                cmd: shlex_join(main_cmd),
                path,
            }
        }
        Some((head, tail)) if head == "tree" => {
            let path = first_non_flag_operand(
                tail,
                &["-L", "-P", "-I", "--charset", "--filelimit", "--sort"],
            )
            .map(|p| short_display_path(&p));
            ParsedCommand::ListFiles {
                cmd: shlex_join(main_cmd),
                path,
            }
        }
        Some((head, tail)) if head == "du" => {
            let path = first_non_flag_operand(
                tail,
                &[
                    "-d",
                    "--max-depth",
                    "-B",
                    "--block-size",
                    "--exclude",
                    "--time-style",
                ],
            )
            .map(|p| short_display_path(&p));
            ParsedCommand::ListFiles {
                cmd: shlex_join(main_cmd),
                path,
            }
        }
        Some((head, tail)) if head == "rg" || head == "rga" || head == "ripgrep-all" => {
            let args_no_connector = trim_at_connector(tail);
            let has_files_flag = args_no_connector.iter().any(|a| a == "--files");
            let candidates = skip_flag_values(
                &args_no_connector,
                &[
                    "-g",
                    "--glob",
                    "--iglob",
                    "-t",
                    "--type",
                    "--type-add",
                    "--type-not",
                    "-m",
                    "--max-count",
                    "-A",
                    "-B",
                    "-C",
                    "--context",
                    "--max-depth",
                ],
            );
            let non_flags: Vec<&String> = candidates
                .into_iter()
                .filter(|p| !p.starts_with('-'))
                .collect();
            if has_files_flag {
                let path = non_flags.first().map(|s| short_display_path(s));
                ParsedCommand::ListFiles {
                    cmd: shlex_join(main_cmd),
                    path,
                }
            } else {
                let query = non_flags.first().cloned().map(String::from);
                let path = non_flags.get(1).map(|s| short_display_path(s));
                ParsedCommand::Search {
                    cmd: shlex_join(main_cmd),
                    query,
                    path,
                }
            }
        }
        Some((head, tail)) if head == "git" => match tail.split_first() {
            Some((subcmd, sub_tail)) if subcmd == "grep" => parse_grep_like(main_cmd, sub_tail),
            Some((subcmd, sub_tail)) if subcmd == "ls-files" => {
                let path = first_non_flag_operand(
                    sub_tail,
                    &["--exclude", "--exclude-from", "--pathspec-from-file"],
                )
                .map(|p| short_display_path(&p));
                ParsedCommand::ListFiles {
                    cmd: shlex_join(main_cmd),
                    path,
                }
            }
            _ => ParsedCommand::Unknown {
                cmd: shlex_join(main_cmd),
            },
        },
        Some((head, tail)) if head == "fd" => {
            let (query, path) = parse_fd_query_and_path(tail);
            if query.is_some() {
                ParsedCommand::Search {
                    cmd: shlex_join(main_cmd),
                    query,
                    path,
                }
            } else {
                ParsedCommand::ListFiles {
                    cmd: shlex_join(main_cmd),
                    path,
                }
            }
        }
        Some((head, tail)) if head == "find" => {
            // Basic find support: capture path and common name filter
            let (query, path) = parse_find_query_and_path(tail);
            if query.is_some() {
                ParsedCommand::Search {
                    cmd: shlex_join(main_cmd),
                    query,
                    path,
                }
            } else {
                ParsedCommand::ListFiles {
                    cmd: shlex_join(main_cmd),
                    path,
                }
            }
        }
        Some((head, tail)) if matches!(head.as_str(), "grep" | "egrep" | "fgrep") => {
            parse_grep_like(main_cmd, tail)
        }
        Some((head, tail)) if matches!(head.as_str(), "ag" | "ack" | "pt") => {
            let args_no_connector = trim_at_connector(tail);
            let candidates = skip_flag_values(
                &args_no_connector,
                &[
                    "-G",
                    "-g",
                    "--file-search-regex",
                    "--ignore-dir",
                    "--ignore-file",
                    "--path-to-ignore",
                ],
            );
            let non_flags: Vec<&String> = candidates
                .into_iter()
                .filter(|p| !p.starts_with('-'))
                .collect();
            let query = non_flags.first().cloned().map(String::from);
            let path = non_flags.get(1).map(|s| short_display_path(s));
            ParsedCommand::Search {
                cmd: shlex_join(main_cmd),
                query,
                path,
            }
        }
        Some((head, tail)) if head == "cat" => {
            if let Some(path) = single_non_flag_operand(tail, &[]) {
                let name = short_display_path(&path);
                ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        Some((head, tail)) if matches!(head.as_str(), "bat" | "batcat") => {
            if let Some(path) = single_non_flag_operand(
                tail,
                &[
                    "--theme",
                    "--language",
                    "--style",
                    "--terminal-width",
                    "--tabs",
                    "--line-range",
                    "--map-syntax",
                ],
            ) {
                let name = short_display_path(&path);
                ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        Some((head, tail)) if head == "less" => {
            if let Some(path) = single_non_flag_operand(
                tail,
                &[
                    "-p",
                    "-P",
                    "-x",
                    "-y",
                    "-z",
                    "-j",
                    "--pattern",
                    "--prompt",
                    "--tabs",
                    "--shift",
                    "--jump-target",
                ],
            ) {
                let name = short_display_path(&path);
                ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        Some((head, tail)) if head == "more" => {
            if let Some(path) = single_non_flag_operand(tail, &[]) {
                let name = short_display_path(&path);
                ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        Some((head, tail)) if head == "head" => {
            // Support `head -n 50 file` and `head -n50 file` forms.
            let has_valid_n = match tail.split_first() {
                Some((first, rest)) if first == "-n" => rest
                    .first()
                    .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit())),
                Some((first, _)) if first.starts_with("-n") => {
                    first[2..].chars().all(|c| c.is_ascii_digit())
                }
                _ => false,
            };
            if has_valid_n {
                // Build candidates skipping the numeric value consumed by `-n` when separated.
                let mut candidates: Vec<&String> = Vec::new();
                let mut i = 0;
                while i < tail.len() {
                    if i == 0 && tail[i] == "-n" && i + 1 < tail.len() {
                        let n = &tail[i + 1];
                        if n.chars().all(|c| c.is_ascii_digit()) {
                            i += 2;
                            continue;
                        }
                    }
                    candidates.push(&tail[i]);
                    i += 1;
                }
                if let Some(p) = candidates.into_iter().find(|p| !p.starts_with('-')) {
                    let path = p.clone();
                    let name = short_display_path(&path);
                    return ParsedCommand::Read {
                        cmd: shlex_join(main_cmd),
                        name,
                        path: PathBuf::from(path),
                    };
                }
            }
            if let [path] = tail
                && !path.starts_with('-')
            {
                let name = short_display_path(path);
                return ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                };
            }
            ParsedCommand::Unknown {
                cmd: shlex_join(main_cmd),
            }
        }
        Some((head, tail)) if head == "tail" => {
            // Support `tail -n +10 file` and `tail -n+10 file` forms.
            let has_valid_n = match tail.split_first() {
                Some((first, rest)) if first == "-n" => rest.first().is_some_and(|n| {
                    let s = n.strip_prefix('+').unwrap_or(n);
                    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
                }),
                Some((first, _)) if first.starts_with("-n") => {
                    let v = &first[2..];
                    let s = v.strip_prefix('+').unwrap_or(v);
                    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
                }
                _ => false,
            };
            if has_valid_n {
                // Build candidates skipping the numeric value consumed by `-n` when separated.
                let mut candidates: Vec<&String> = Vec::new();
                let mut i = 0;
                while i < tail.len() {
                    if i == 0 && tail[i] == "-n" && i + 1 < tail.len() {
                        let n = &tail[i + 1];
                        let s = n.strip_prefix('+').unwrap_or(n);
                        if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
                            i += 2;
                            continue;
                        }
                    }
                    candidates.push(&tail[i]);
                    i += 1;
                }
                if let Some(p) = candidates.into_iter().find(|p| !p.starts_with('-')) {
                    let path = p.clone();
                    let name = short_display_path(&path);
                    return ParsedCommand::Read {
                        cmd: shlex_join(main_cmd),
                        name,
                        path: PathBuf::from(path),
                    };
                }
            }
            if let [path] = tail
                && !path.starts_with('-')
            {
                let name = short_display_path(path);
                return ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                };
            }
            ParsedCommand::Unknown {
                cmd: shlex_join(main_cmd),
            }
        }
        Some((head, tail)) if head == "awk" => {
            if let Some(path) = awk_data_file_operand_inner(tail) {
                let name = short_display_path(&path);
                ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        Some((head, tail)) if head == "nl" => {
            // Avoid treating option values as paths (e.g., nl -s "  ").
            let candidates = skip_flag_values(tail, &["-s", "-w", "-v", "-i", "-b"]);
            if let Some(p) = candidates.into_iter().find(|p| !p.starts_with('-')) {
                let path = p.clone();
                let name = short_display_path(&path);
                ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        Some((head, tail)) if head == "sed" => {
            if let Some(path) = sed_read_path(tail) {
                let name = short_display_path(&path);
                ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        Some((head, tail)) if is_python_command(head) => {
            if python_walks_files(tail) {
                ParsedCommand::ListFiles {
                    cmd: shlex_join(main_cmd),
                    path: None,
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        // Other commands
        _ => ParsedCommand::Unknown {
            cmd: shlex_join(main_cmd),
        },
    }
}
