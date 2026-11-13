//! Where a shell command goes, judged by the programs it runs, never by what
//! they are given.

use std::path::Path;

use super::Station;
use super::check::{self, CheckKind};

const NEUTRAL: &[&str] = &[
    "echo", "printf", "cd", "pushd", "popd", "export", "true", "false", "set", "pwd", "sleep",
    "which", "type", "test", "[", "exit",
];
const FILES: &[&str] = &[
    "cat", "head", "tail", "less", "sed", "grep", "rg", "find", "fd", "ls", "wc", "cp", "mv",
    "mkdir", "touch", "diff", "cmp", "sort", "uniq", "cut", "tr", "awk", "tee", "stat", "file",
    "du", "tree", "ln", "basename", "dirname", "realpath", "readlink", "jq", "rm", "rmdir",
    "chmod",
];
const NETWORK: &[&str] = &[
    "curl", "wget", "gh", "ssh", "scp", "sftp", "rsync", "nc", "ping", "dig",
];
/// Programs that reach the network only for some subcommands.
const NETWORK_SUBCOMMANDS: &[(&str, &[&str])] = &[
    ("git", &["push", "pull", "fetch", "clone", "ls-remote"]),
    (
        "cargo",
        &["install", "fetch", "publish", "update", "search", "login"],
    ),
    ("pip", &["install", "download"]),
    ("pip3", &["install", "download"]),
    ("bun", &["install", "i", "add", "update", "publish"]),
    ("podman", &["pull", "push", "login", "search"]),
    ("upkg", &["install", "upgrade", "search", "--self-upgrade"]),
    ("blackship", &["bootstrap", "migrate"]),
];
/// Flags that consume the next word, for programs whose subcommand follows them.
const FLAGS_WITH_VALUE: &[(&str, &[&str])] = &[
    (
        "git",
        &["-C", "-c", "--git-dir", "--work-tree", "--namespace"],
    ),
    (
        "podman",
        &[
            "-c",
            "--connection",
            "--url",
            "--log-level",
            "--root",
            "--runroot",
        ],
    ),
    ("blackship", &["-c", "--config", "--format"]),
];
/// Programs that run another command, and their flags that consume a value.
const WRAPPERS: &[(&str, &[&str])] = &[
    (
        "sudo",
        &["-u", "-g", "-h", "-p", "-C", "-D", "-r", "-t", "-U"],
    ),
    ("doas", &["-u", "-C"]),
    ("env", &["-u", "-C", "-S"]),
    ("time", &["-f", "-o"]),
    ("timeout", &["-s", "-k", "--signal", "--kill-after"]),
    ("nohup", &[]),
    ("nice", &["-n"]),
    ("ionice", &["-c", "-n", "-p"]),
    ("stdbuf", &["-i", "-o", "-e"]),
    ("caffeinate", &["-t", "-w"]),
    ("command", &[]),
    ("exec", &["-a"]),
    ("xargs", &["-I", "-L", "-n", "-P", "-s", "-E", "-d", "-a"]),
];
const MAX_WRAPPERS: usize = 6;

/// What a command line is, as the bus may tell it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Command {
    pub station: Station,
    pub label: String,
    pub check: Option<CheckKind>,
}

/// The station, the name of the program that matters and the check it runs;
/// nothing past them.
pub(crate) fn classify(argv: &[String]) -> Command {
    let commands = match chaos_sh::bash::extract_bash_command(argv) {
        Some((_, script)) => chaos_sh::bash::every_command_words(script),
        None => vec![argv.to_vec()],
    };
    let runs: Vec<Vec<String>> = commands
        .iter()
        .flat_map(|words| run_commands(words))
        .collect();
    let check = runs
        .iter()
        .filter_map(|words| check::kind(&program_name(&words[0]), &words[1..]))
        .max();
    let programs: Vec<(String, Option<Station>)> = runs
        .iter()
        .map(|words| {
            let program = program_name(&words[0]);
            let station = station(&program, &words[1..]);
            (program, station)
        })
        .collect();
    let decided: Vec<&(String, Option<Station>)> = programs
        .iter()
        .filter(|(_, station)| station.is_some())
        .collect();
    let station = if decided
        .iter()
        .any(|(_, station)| *station == Some(Station::Network))
    {
        Station::Network
    } else if !decided.is_empty()
        && decided
            .iter()
            .all(|(_, station)| *station == Some(Station::Files))
    {
        Station::Files
    } else {
        Station::Shell
    };
    let label = decided
        .first()
        .map(|(program, _)| program)
        .or_else(|| programs.first().map(|(program, _)| program))
        .cloned()
        .unwrap_or_else(|| "command".to_string());
    Command {
        station,
        label,
        check,
    }
}

/// The commands a command line actually runs: wrappers and leading
/// assignments peeled, and `find -exec` counted as its own command.
fn run_commands(words: &[String]) -> Vec<Vec<String>> {
    let mut rest: &[String] = words;
    for _ in 0..MAX_WRAPPERS {
        while rest.first().is_some_and(|word| is_assignment(word)) {
            rest = &rest[1..];
        }
        let Some(first) = rest.first() else {
            return Vec::new();
        };
        let program = program_name(first);
        if program == "command"
            && rest
                .get(1)
                .is_some_and(|flag| matches!(flag.as_str(), "-v" | "-V"))
        {
            break;
        }
        if let Some(inner) = runner_exec(&program, &rest[1..]) {
            rest = inner;
            continue;
        }
        let Some((_, value_flags)) = WRAPPERS.iter().find(|(name, _)| *name == program) else {
            break;
        };
        rest = skip_options(&rest[1..], value_flags);
        if program == "timeout" && !rest.is_empty() {
            rest = &rest[1..];
        }
    }
    if rest.is_empty() {
        return Vec::new();
    }
    let mut commands = vec![rest.to_vec()];
    if program_name(&rest[0]) == "find"
        && let Some(at) = rest
            .iter()
            .position(|word| matches!(word.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir"))
    {
        let inner: Vec<String> = rest[at + 1..]
            .iter()
            .take_while(|word| !matches!(word.as_str(), ";" | "\\;" | "+"))
            .cloned()
            .collect();
        commands.extend(run_commands(&inner));
    }
    commands
}

/// `bundle exec cmd` and `mise exec -- cmd` run `cmd`.
fn runner_exec<'a>(program: &str, args: &'a [String]) -> Option<&'a [String]> {
    match (program, args.first().map(String::as_str)) {
        ("bundle", Some("exec")) => Some(&args[1..]),
        ("mise", Some("exec" | "x")) => {
            let at = args.iter().position(|arg| arg == "--")?;
            Some(&args[at + 1..])
        }
        _ => None,
    }
}

fn skip_options<'a>(mut words: &'a [String], value_flags: &[&str]) -> &'a [String] {
    while let Some(word) = words.first() {
        if word == "--" {
            return &words[1..];
        }
        if is_assignment(word) {
            words = &words[1..];
            continue;
        }
        if !word.starts_with('-') || word == "-" {
            break;
        }
        let takes_value = value_flags.contains(&word.as_str());
        words = &words[if takes_value { 2.min(words.len()) } else { 1 }..];
    }
    words
}

fn station(program: &str, args: &[String]) -> Option<Station> {
    if NEUTRAL.contains(&program) {
        return None;
    }
    if NETWORK.contains(&program) || reaches_network(program, args) {
        return Some(Station::Network);
    }
    if FILES.contains(&program) {
        return Some(Station::Files);
    }
    Some(Station::Shell)
}

fn reaches_network(program: &str, args: &[String]) -> bool {
    match program {
        "python" | "python3" if args.first().is_some_and(|flag| flag == "-m") => args
            .get(1)
            .is_some_and(|module| reaches_network(module, &args[2..])),
        _ => {
            let Some((_, subcommands)) = NETWORK_SUBCOMMANDS
                .iter()
                .find(|(name, _)| *name == program)
            else {
                return false;
            };
            if args
                .iter()
                .any(|arg| arg.starts_with("--") && subcommands.contains(&arg.as_str()))
            {
                return true;
            }
            let value_flags = FLAGS_WITH_VALUE
                .iter()
                .find(|(name, _)| *name == program)
                .map_or(&[][..], |(_, flags)| *flags);
            let rest = skip_options(args, value_flags);
            let Some(sub) = rest.first() else {
                return false;
            };
            subcommands.contains(&sub.as_str())
        }
    }
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !name.starts_with(|c: char| c.is_ascii_digit())
    })
}

fn program_name(word: &str) -> String {
    let name = Path::new(word)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(word)
        .to_ascii_lowercase();
    for suffix in [".exe", ".cmd", ".bat"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    name
}

#[cfg(test)]
mod tests;
