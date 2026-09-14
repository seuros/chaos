use super::*;
use std::string::ToString;

fn vec_str(args: &[&str]) -> Vec<String> {
    args.iter().map(ToString::to_string).collect()
}

#[test]
fn known_safe_examples() {
    assert!(is_safe_to_call_with_exec(&vec_str(&["ls"])));
    assert!(is_safe_to_call_with_exec(&vec_str(&["git", "status"])));
    assert!(is_safe_to_call_with_exec(&vec_str(&["git", "branch"])));
    assert!(is_safe_to_call_with_exec(&vec_str(&[
        "git",
        "branch",
        "--show-current"
    ])));
    assert!(is_safe_to_call_with_exec(&vec_str(&["base64"])));
    assert!(is_safe_to_call_with_exec(&vec_str(&[
        "sed", "-n", "1,5p", "file.txt"
    ])));
    assert!(is_safe_to_call_with_exec(&vec_str(&[
        "nl",
        "-nrz",
        "Cargo.toml"
    ])));

    // Safe `find` command (no unsafe options).
    assert!(is_safe_to_call_with_exec(&vec_str(&[
        "find", ".", "-name", "file.txt"
    ])));

    if cfg!(target_os = "linux") {
        assert!(is_safe_to_call_with_exec(&vec_str(&["numfmt", "1000"])));
        assert!(is_safe_to_call_with_exec(&vec_str(&["tac", "Cargo.toml"])));
    } else {
        assert!(!is_safe_to_call_with_exec(&vec_str(&["numfmt", "1000"])));
        assert!(!is_safe_to_call_with_exec(&vec_str(&["tac", "Cargo.toml"])));
    }
}

#[test]
fn git_branch_mutating_flags_are_not_safe() {
    assert!(!is_known_safe_command(&vec_str(&[
        "git", "branch", "-d", "feature"
    ])));
    assert!(!is_known_safe_command(&vec_str(&[
        "git",
        "branch",
        "new-branch"
    ])));
}

#[test]
fn git_branch_global_options_respect_safety_rules() {
    use pretty_assertions::assert_eq;

    assert_eq!(
        is_known_safe_command(&vec_str(&["git", "-C", ".", "branch", "--show-current"])),
        true
    );
    assert_eq!(
        is_known_safe_command(&vec_str(&["git", "-C", ".", "branch", "-d", "feature"])),
        false
    );
    assert_eq!(
        is_known_safe_command(&vec_str(&["bash", "-lc", "git -C . branch -d feature",])),
        false
    );
}

#[test]
fn git_first_positional_is_the_subcommand() {
    // In git, the first non-option token is the subcommand. Later positional
    // args (like branch names) must not be treated as subcommands.
    assert!(!is_known_safe_command(&vec_str(&[
        "git", "checkout", "status",
    ])));
}

#[test]
fn git_output_and_config_override_flags_are_not_safe() {
    assert!(!is_known_safe_command(&vec_str(&[
        "git",
        "log",
        "--output=/tmp/git-log-out-test",
        "-n",
        "1",
    ])));
    assert!(!is_known_safe_command(&vec_str(&[
        "git",
        "diff",
        "--output",
        "/tmp/git-diff-out-test",
    ])));
    assert!(!is_known_safe_command(&vec_str(&[
        "git",
        "show",
        "--output=/tmp/git-show-out-test",
        "HEAD",
    ])));
    assert!(!is_known_safe_command(&vec_str(&[
        "git",
        "-c",
        "core.pager=cat",
        "log",
        "-n",
        "1",
    ])));
    assert!(!is_known_safe_command(&vec_str(&[
        "git",
        "-ccore.pager=cat",
        "status",
    ])));
}

#[test]
fn cargo_check_is_not_safe() {
    assert!(!is_known_safe_command(&vec_str(&["cargo", "check"])));
}

#[test]
fn zsh_lc_safe_command_sequence() {
    assert!(is_known_safe_command(&vec_str(&["zsh", "-lc", "ls"])));
}

#[test]
fn unknown_or_partial() {
    assert!(!is_safe_to_call_with_exec(&vec_str(&["foo"])));
    assert!(!is_safe_to_call_with_exec(&vec_str(&["git", "fetch"])));
    assert!(!is_safe_to_call_with_exec(&vec_str(&[
        "sed", "-n", "xp", "file.txt"
    ])));

    // Unsafe `find` commands.
    for args in [
        vec_str(&["find", ".", "-name", "file.txt", "-exec", "rm", "{}", ";"]),
        vec_str(&["find", ".", "-name", "*.txt", "-execdir", "sh", "{}", ";"]),
        vec_str(&["find", ".", "-name", "file.txt", "-ok", "rm", "{}", ";"]),
        vec_str(&["find", ".", "-name", "*.txt", "-okdir", "sh", "{}", ";"]),
        vec_str(&["find", ".", "-delete", "-name", "file.txt"]),
        vec_str(&["find", ".", "-fls", "/etc/passwd"]),
        vec_str(&["find", ".", "-fprint", "/etc/passwd"]),
        vec_str(&["find", ".", "-fprint0", "/etc/passwd"]),
        vec_str(&["find", ".", "-fprintf", "/root/suid.txt", "%#m %u %p\n"]),
    ] {
        assert!(
            !is_safe_to_call_with_exec(&args),
            "expected {args:?} to be unsafe"
        );
    }
}

#[test]
fn base64_output_options_are_unsafe() {
    for args in [
        vec_str(&["base64", "-o", "out.bin"]),
        vec_str(&["base64", "--output", "out.bin"]),
        vec_str(&["base64", "--output=out.bin"]),
        vec_str(&["base64", "-ob64.txt"]),
    ] {
        assert!(
            !is_safe_to_call_with_exec(&args),
            "expected {args:?} to be considered unsafe due to output option"
        );
    }
}

#[test]
fn ripgrep_rules() {
    // Safe ripgrep invocations – none of the unsafe flags are present.
    assert!(is_safe_to_call_with_exec(&vec_str(&[
        "rg",
        "Cargo.toml",
        "-n"
    ])));

    // Unsafe flags that do not take an argument (present verbatim).
    for args in [
        vec_str(&["rg", "--search-zip", "files"]),
        vec_str(&["rg", "-z", "files"]),
    ] {
        assert!(
            !is_safe_to_call_with_exec(&args),
            "expected {args:?} to be considered unsafe due to zip-search flag",
        );
    }

    // Unsafe flags that expect a value, provided in both split and = forms.
    for args in [
        vec_str(&["rg", "--pre", "pwned", "files"]),
        vec_str(&["rg", "--pre=pwned", "files"]),
        vec_str(&["rg", "--hostname-bin", "pwned", "files"]),
        vec_str(&["rg", "--hostname-bin=pwned", "files"]),
    ] {
        assert!(
            !is_safe_to_call_with_exec(&args),
            "expected {args:?} to be considered unsafe due to external-command flag",
        );
    }
}

#[test]
fn bash_lc_safe_examples() {
    assert!(is_known_safe_command(&vec_str(&["bash", "-lc", "ls"])));
    assert!(is_known_safe_command(&vec_str(&["bash", "-lc", "ls -1"])));
    assert!(is_known_safe_command(&vec_str(&[
        "bash",
        "-lc",
        "git status"
    ])));
    assert!(is_known_safe_command(&vec_str(&[
        "bash",
        "-lc",
        "grep -R \"Cargo.toml\" -n"
    ])));
    assert!(is_known_safe_command(&vec_str(&[
        "bash",
        "-lc",
        "sed -n 1,5p file.txt"
    ])));
    assert!(is_known_safe_command(&vec_str(&[
        "bash",
        "-lc",
        "sed -n '1,5p' file.txt"
    ])));

    assert!(is_known_safe_command(&vec_str(&[
        "bash",
        "-lc",
        "find . -name file.txt"
    ])));
}

#[test]
fn bash_lc_safe_examples_with_operators() {
    assert!(is_known_safe_command(&vec_str(&[
        "bash",
        "-lc",
        "grep -R \"Cargo.toml\" -n || true"
    ])));
    assert!(is_known_safe_command(&vec_str(&[
        "bash",
        "-lc",
        "ls && pwd"
    ])));
    assert!(is_known_safe_command(&vec_str(&[
        "bash",
        "-lc",
        "echo 'hi' ; ls"
    ])));
    assert!(is_known_safe_command(&vec_str(&[
        "bash",
        "-lc",
        "ls | wc -l"
    ])));
}

#[test]
fn bash_lc_unsafe_examples() {
    assert!(
        !is_known_safe_command(&vec_str(&["bash", "-lc", "git", "status"])),
        "Four arg version is not known to be safe."
    );
    assert!(
        !is_known_safe_command(&vec_str(&["bash", "-lc", "'git status'"])),
        "The extra quoting around 'git status' makes it a program named 'git status' and is therefore unsafe."
    );

    assert!(
        !is_known_safe_command(&vec_str(&["bash", "-lc", "find . -name file.txt -delete"])),
        "Unsafe find option should not be auto-approved."
    );

    // Disallowed because of unsafe command in sequence.
    assert!(
        !is_known_safe_command(&vec_str(&["bash", "-lc", "ls && rm -rf /"])),
        "Sequence containing unsafe command must be rejected"
    );

    // Disallowed because of parentheses / subshell.
    assert!(
        !is_known_safe_command(&vec_str(&["bash", "-lc", "(ls)"])),
        "Parentheses (subshell) are not provably safe with the current parser"
    );
    assert!(
        !is_known_safe_command(&vec_str(&["bash", "-lc", "ls || (pwd && echo hi)"])),
        "Nested parentheses are not provably safe with the current parser"
    );

    // Disallowed redirection.
    assert!(
        !is_known_safe_command(&vec_str(&["bash", "-lc", "ls > out.txt"])),
        "> redirection should be rejected"
    );
}
