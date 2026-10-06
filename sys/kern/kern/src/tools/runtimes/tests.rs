use super::*;
use crate::config::types::ShellEnvironmentPolicy;
use crate::shell::ShellType;
use crate::shell_environment::ShellEnvironment;
use pretty_assertions::assert_eq;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::watch;

fn shell_with_environment(vars: HashMap<String, String>, environment_cwd: PathBuf) -> Shell {
    let (_tx, shell_environment) = watch::channel(Some(Arc::new(ShellEnvironment {
        vars,
        cwd: environment_cwd,
    })));
    Shell {
        shell_type: ShellType::Zsh,
        shell_path: PathBuf::from("/bin/zsh"),
        shell_environment,
    }
}

fn login_command() -> Vec<String> {
    vec![
        "/bin/zsh".to_string(),
        "-lc".to_string(),
        "printf '%s' \"$PATH\"".to_string(),
        "arg0".to_string(),
    ]
}

#[test]
fn captured_environment_replaces_login_profile_loading() {
    let dir = tempdir().expect("create temp dir");
    let shell = shell_with_environment(
        HashMap::from([("PATH".to_string(), "/captured/bin".to_string())]),
        dir.path().to_path_buf(),
    );
    let process_id = ProcessId::new();
    let process_id_string = process_id.to_string();

    let (command, env) = maybe_apply_shell_environment(
        &login_command(),
        &shell,
        dir.path(),
        &ShellEnvironmentPolicy::default(),
        process_id,
        &HashMap::new(),
        &HashMap::new(),
    );

    assert_eq!(command[0], "/bin/zsh");
    assert_eq!(command[1], "-c");
    assert_eq!(command[2], "printf '%s' \"$PATH\"");
    assert_eq!(command[3], "arg0");
    assert_eq!(env.get("PATH").map(String::as_str), Some("/captured/bin"));
    assert_eq!(
        env.get(crate::exec_env::CHAOS_THREAD_ID_ENV_VAR)
            .map(String::as_str),
        Some(process_id_string.as_str())
    );
}

#[test]
fn sensitive_variables_are_filtered_by_default() {
    let dir = tempdir().expect("create temp dir");
    let shell = shell_with_environment(
        HashMap::from([
            ("PATH".to_string(), "/captured/bin".to_string()),
            ("API_KEY".to_string(), "must-not-leak".to_string()),
            ("ACCESS_TOKEN".to_string(), "must-not-leak".to_string()),
        ]),
        dir.path().to_path_buf(),
    );

    let (_, env) = maybe_apply_shell_environment(
        &login_command(),
        &shell,
        dir.path(),
        &ShellEnvironmentPolicy::default(),
        ProcessId::new(),
        &HashMap::new(),
        &HashMap::new(),
    );

    assert!(!env.contains_key("API_KEY"));
    assert!(!env.contains_key("ACCESS_TOKEN"));
    assert_eq!(env.get("PATH").map(String::as_str), Some("/captured/bin"));
}

#[test]
fn sensitive_variables_require_explicit_opt_in() {
    let dir = tempdir().expect("create temp dir");
    let shell = shell_with_environment(
        HashMap::from([("API_KEY".to_string(), "allowed".to_string())]),
        dir.path().to_path_buf(),
    );
    let policy = ShellEnvironmentPolicy {
        ignore_default_excludes: true,
        ..Default::default()
    };

    let (_, env) = maybe_apply_shell_environment(
        &login_command(),
        &shell,
        dir.path(),
        &policy,
        ProcessId::new(),
        &HashMap::new(),
        &HashMap::new(),
    );

    assert_eq!(env.get("API_KEY").map(String::as_str), Some("allowed"));
}

#[test]
fn explicit_overrides_win_without_entering_command_arguments() {
    let dir = tempdir().expect("create temp dir");
    let shell = shell_with_environment(
        HashMap::from([("PATH".to_string(), "/captured/bin".to_string())]),
        dir.path().to_path_buf(),
    );
    let overrides = HashMap::from([
        ("PATH".to_string(), "/configured/bin".to_string()),
        ("API_KEY".to_string(), "explicit-secret".to_string()),
    ]);

    let (command, env) = maybe_apply_shell_environment(
        &login_command(),
        &shell,
        dir.path(),
        &ShellEnvironmentPolicy::default(),
        ProcessId::new(),
        &HashMap::new(),
        &overrides,
    );

    assert_eq!(env.get("PATH").map(String::as_str), Some("/configured/bin"));
    assert_eq!(
        env.get("API_KEY").map(String::as_str),
        Some("explicit-secret")
    );
    assert!(
        command
            .iter()
            .all(|argument| !argument.contains("explicit-secret"))
    );
}

#[test]
fn cwd_mismatch_uses_non_login_command_and_fallback_environment() {
    let dir = tempdir().expect("create temp dir");
    let captured_cwd = dir.path().join("captured");
    let command_cwd = dir.path().join("command");
    std::fs::create_dir_all(&captured_cwd).expect("create captured cwd");
    std::fs::create_dir_all(&command_cwd).expect("create command cwd");
    let shell = shell_with_environment(
        HashMap::from([("PATH".to_string(), "/captured/bin".to_string())]),
        captured_cwd,
    );
    let fallback = HashMap::from([("PATH".to_string(), "/fallback/bin".to_string())]);
    let original = login_command();

    let (command, env) = maybe_apply_shell_environment(
        &original,
        &shell,
        &command_cwd,
        &ShellEnvironmentPolicy::default(),
        ProcessId::new(),
        &fallback,
        &HashMap::new(),
    );

    assert_eq!(command[0], original[0]);
    assert_eq!(command[1], "-c");
    assert_eq!(command[2..], original[2..]);
    assert_eq!(env, fallback);
}

#[test]
fn explicit_profile_mode_bypasses_captured_environment() {
    let dir = tempdir().expect("create temp dir");
    let shell = shell_with_environment(
        HashMap::from([("PATH".to_string(), "/captured/bin".to_string())]),
        dir.path().to_path_buf(),
    );
    let fallback = HashMap::from([("PATH".to_string(), "/fallback/bin".to_string())]);
    let policy = ShellEnvironmentPolicy {
        use_profile: true,
        ..Default::default()
    };
    let original = login_command();

    let (command, env) = maybe_apply_shell_environment(
        &original,
        &shell,
        dir.path(),
        &policy,
        ProcessId::new(),
        &fallback,
        &HashMap::new(),
    );

    assert_eq!(command, original);
    assert_eq!(env, fallback);
}

#[test]
fn non_login_commands_are_not_changed() {
    let dir = tempdir().expect("create temp dir");
    let shell = shell_with_environment(
        HashMap::from([("PATH".to_string(), "/captured/bin".to_string())]),
        dir.path().to_path_buf(),
    );
    let fallback = HashMap::from([("PATH".to_string(), "/fallback/bin".to_string())]);
    let original = vec![
        "/bin/zsh".to_string(),
        "-c".to_string(),
        "echo hello".to_string(),
    ];

    let (command, env) = maybe_apply_shell_environment(
        &original,
        &shell,
        dir.path(),
        &ShellEnvironmentPolicy::default(),
        ProcessId::new(),
        &fallback,
        &HashMap::new(),
    );

    assert_eq!(command, original);
    assert_eq!(env, fallback);
}
