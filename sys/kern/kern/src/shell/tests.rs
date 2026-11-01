use super::*;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn can_run_on_shell_test() {
    let cmd = "echo \"Works\"";
    assert!(shell_works(Some(ultimate_fallback_shell()), cmd, true));
    assert!(shell_works(get_shell(ShellType::Zsh, None), cmd, false));
    assert!(shell_works(get_shell(ShellType::Bash, None), cmd, true));
    assert!(shell_works(get_shell(ShellType::Sh, None), cmd, true));
}

fn shell_works(shell: Option<Shell>, command: &str, required: bool) -> bool {
    if let Some(shell) = shell {
        assert_eq!(
            shell.shell_path.file_name().and_then(|name| name.to_str()),
            Some(shell.name()),
            "shell path: {:?}",
            shell.shell_path
        );
        let args = shell.derive_exec_args(command, false);
        let output = Command::new(args[0].clone())
            .args(&args[1..])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("Works"));
        true
    } else {
        !required
    }
}

#[test]
fn derive_exec_args() {
    for (shell_type, path) in [(ShellType::Bash, "/bin/bash"), (ShellType::Zsh, "/bin/zsh")] {
        let shell = Shell {
            shell_type,
            shell_path: PathBuf::from(path),
            version: None,
            shell_environment: empty_shell_environment_receiver(),
        };
        for (login, flag) in [(false, "-c"), (true, "-lc")] {
            assert_eq!(
                shell.derive_exec_args("echo hello", login),
                vec![path, flag, "echo hello"]
            );
        }
    }
}

#[test]
fn default_prefers_bash_over_passwd_shell() {
    let Some(bash) = get_shell(ShellType::Bash, None) else {
        return;
    };
    let shell = default_user_shell_from_path(Some(PathBuf::from("/bin/zsh")));
    assert_eq!(shell.shell_type, ShellType::Bash);
    assert_eq!(shell.shell_path, bash.shell_path);
}

#[test]
fn configured_name_overrides_default() {
    if get_shell(ShellType::Zsh, None).is_none() {
        return;
    }
    let shell = resolve_user_shell_from_path(Some("zsh"), None);
    assert_eq!(shell.shell_type, ShellType::Zsh);
}

#[test]
fn configured_absolute_path_is_honored() {
    let shell = resolve_user_shell_from_path(Some("/bin/sh"), None);
    assert_eq!(shell.shell_type, ShellType::Sh);
    assert_eq!(shell.shell_path, PathBuf::from("/bin/sh"));
}

#[test]
fn unavailable_configured_shell_falls_back_to_default() {
    let expected = default_user_shell_from_path(None);
    for value in ["/nonexistent/bash", "fish", ""] {
        let shell = resolve_user_shell_from_path(Some(value), None);
        assert_eq!(shell.shell_path, expected.shell_path, "value: {value:?}");
    }
}

#[test]
fn parses_shell_version_banners() {
    assert_eq!(
        parse_shell_version("GNU bash, version 3.2.57(1)-release (arm64-apple-darwin23)\n"),
        Some("3.2.57".to_string())
    );
    assert_eq!(
        parse_shell_version("zsh 5.9 (arm64-apple-darwin23.0)\n"),
        Some("5.9".to_string())
    );
    assert_eq!(parse_shell_version("usage: sh [options]\n"), None);
    assert_eq!(parse_shell_version(""), None);
}
