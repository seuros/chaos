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

#[tokio::test]
async fn test_current_shell_detects_zsh() {
    let shell = Command::new("sh")
        .arg("-c")
        .arg("echo $SHELL")
        .output()
        .unwrap();

    let shell_path = String::from_utf8_lossy(&shell.stdout).trim().to_string();
    if shell_path.ends_with("/zsh") {
        assert_eq!(
            default_user_shell(),
            Shell {
                shell_type: ShellType::Zsh,
                shell_path: PathBuf::from(shell_path),
                shell_environment: empty_shell_environment_receiver(),
            }
        );
    }
}
