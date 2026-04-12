//! Path and operand extraction helpers that operate on structured token data.

/// Shorten a path to the last component, excluding `build`/`dist`/`node_modules`/`src`.
/// It also pulls out a useful path from a directory such as:
/// - webview/src -> webview
/// - foo/src/ -> foo
/// - packages/app/node_modules/ -> app
pub fn short_display_path(path: &str) -> String {
    // Normalize separators and drop any trailing slash for display.
    let normalized = path.replace('\\', "/");
    let trimmed = normalized.trim_end_matches('/');
    let mut parts = trimmed.split('/').rev().filter(|p| {
        !p.is_empty() && *p != "build" && *p != "dist" && *p != "node_modules" && *p != "src"
    });
    parts
        .next()
        .map(str::to_string)
        .unwrap_or_else(|| trimmed.to_string())
}

// Skip values consumed by specific flags and ignore --flag=value style arguments.
pub fn skip_flag_values<'a>(args: &'a [String], flags_with_vals: &[&str]) -> Vec<&'a String> {
    let mut out: Vec<&'a String> = Vec::new();
    let mut skip_next = false;
    for (i, a) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a == "--" {
            // From here on, everything is positional operands; push the rest and break.
            for rest in &args[i + 1..] {
                out.push(rest);
            }
            break;
        }
        if a.starts_with("--") && a.contains('=') {
            // --flag=value form: treat as a flag taking a value; skip entirely.
            continue;
        }
        if flags_with_vals.contains(&a.as_str()) {
            // This flag consumes the next argument as its value.
            if i + 1 < args.len() {
                skip_next = true;
            }
            continue;
        }
        out.push(a);
    }
    out
}

pub fn first_non_flag_operand(args: &[String], flags_with_vals: &[&str]) -> Option<String> {
    positional_operands(args, flags_with_vals)
        .into_iter()
        .next()
        .cloned()
}

pub fn single_non_flag_operand(args: &[String], flags_with_vals: &[&str]) -> Option<String> {
    let mut operands = positional_operands(args, flags_with_vals).into_iter();
    let first = operands.next()?;
    if operands.next().is_some() {
        return None;
    }
    Some(first.clone())
}

pub fn positional_operands<'a>(args: &'a [String], flags_with_vals: &[&str]) -> Vec<&'a String> {
    let mut out = Vec::new();
    let mut after_double_dash = false;
    let mut skip_next = false;
    for (i, arg) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if after_double_dash {
            out.push(arg);
            continue;
        }
        if arg == "--" {
            after_double_dash = true;
            continue;
        }
        if arg.starts_with("--") && arg.contains('=') {
            continue;
        }
        if flags_with_vals.contains(&arg.as_str()) {
            if i + 1 < args.len() {
                skip_next = true;
            }
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        out.push(arg);
    }
    out
}

/// Returns the data-file operand for an `awk` invocation, if one exists.
/// `args` should be the arguments *after* the `awk` command name.
pub fn awk_data_file_operand_inner(args: &[String]) -> Option<String> {
    if args.is_empty() {
        return None;
    }
    let args_no_connector = super::lexer::trim_at_connector(args);
    let has_script_file = args_no_connector
        .iter()
        .any(|arg| arg == "-f" || arg == "--file");
    let candidates = skip_flag_values(
        &args_no_connector,
        &["-F", "-v", "-f", "--field-separator", "--assign", "--file"],
    );
    let non_flags: Vec<&String> = candidates
        .into_iter()
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    if has_script_file {
        return non_flags.first().cloned().cloned();
    }
    if non_flags.len() >= 2 {
        return Some(non_flags[1].clone());
    }
    None
}

pub fn is_pathish(s: &str) -> bool {
    s == "."
        || s == ".."
        || s.starts_with("./")
        || s.starts_with("../")
        || s.contains('/')
        || s.contains('\\')
}

pub fn is_abs_like(path: &str) -> bool {
    if std::path::Path::new(path).is_absolute() {
        return true;
    }
    let mut chars = path.chars();
    match (chars.next(), chars.next(), chars.next()) {
        // Windows drive path like C:\
        (Some(d), Some(':'), Some('\\')) if d.is_ascii_alphabetic() => return true,
        // UNC path like \\server\share
        (Some('\\'), Some('\\'), _) => return true,
        _ => {}
    }
    false
}

pub fn join_paths(base: &str, rel: &str) -> String {
    if is_abs_like(rel) {
        return rel.to_string();
    }
    if base.is_empty() {
        return rel.to_string();
    }
    let mut buf = std::path::PathBuf::from(base);
    buf.push(rel);
    buf.to_string_lossy().to_string()
}
