/*
Module: runtimes

Concrete ToolRuntime implementations for specific tools. Each runtime stays
small and focused and reuses the orchestrator for approvals + sandbox + retry.
*/
use crate::config::types::ShellEnvironmentPolicy;
use crate::exec::ExecExpiration;
use crate::exec_env::create_env_from;
use crate::path_utils;
use crate::sandboxing::CommandSpec;
use crate::sandboxing::SandboxPermissions;
use crate::shell::Shell;
use crate::tools::sandboxing::ToolError;
use chaos_ipc::ProcessId;
use chaos_ipc::models::PermissionProfile;
use std::collections::HashMap;
use std::path::Path;

pub mod apply_patch;
pub mod shell;
pub mod unified_exec;

/// Shared helper to construct a CommandSpec from a tokenized command line.
/// Validates that at least a program is present.
pub(crate) fn build_command_spec(
    command: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    expiration: ExecExpiration,
    sandbox_permissions: SandboxPermissions,
    additional_permissions: Option<PermissionProfile>,
    justification: Option<String>,
) -> Result<CommandSpec, ToolError> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| ToolError::Rejected("command args are empty".to_string()))?;
    Ok(CommandSpec {
        program: program.clone(),
        args: args.to_vec(),
        cwd: cwd.to_path_buf(),
        env: env.clone(),
        expiration,
        sandbox_permissions,
        additional_permissions,
        justification,
    })
}

/// Use the CWD-aware environment captured from the user's initialized shell.
///
///   shell -lc "<script>"
///   => shell -c "<script>"
///
/// The captured environment is filtered through the active
/// [`ShellEnvironmentPolicy`] before use. Explicit profile mode and non-login
/// shell commands retain their original command. If capture is not ready for
/// the command CWD, the already-filtered fallback environment is used without
/// loading the profile again.
pub(crate) fn maybe_apply_shell_environment(
    command: &[String],
    session_shell: &Shell,
    cwd: &Path,
    policy: &ShellEnvironmentPolicy,
    process_id: ProcessId,
    fallback_env: &HashMap<String, String>,
    explicit_env_overrides: &HashMap<String, String>,
) -> (Vec<String>, HashMap<String, String>) {
    if policy.use_profile {
        return (command.to_vec(), fallback_env.clone());
    }

    if command.len() < 3 || command[1] != "-lc" {
        return (command.to_vec(), fallback_env.clone());
    }

    let mut non_login_command = command.to_vec();
    non_login_command[1] = "-c".to_string();

    let Some(environment) = session_shell.shell_environment() else {
        return (non_login_command, fallback_env.clone());
    };

    let cwd_matches = if let (Ok(environment_cwd), Ok(command_cwd)) = (
        path_utils::normalize_for_path_comparison(environment.cwd.as_path()),
        path_utils::normalize_for_path_comparison(cwd),
    ) {
        environment_cwd == command_cwd
    } else {
        environment.cwd == cwd
    };
    if !cwd_matches {
        return (non_login_command, fallback_env.clone());
    }

    let mut filtered_env = create_env_from(
        environment
            .vars
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
        policy,
        Some(process_id),
    );
    for (key, value) in explicit_env_overrides {
        filtered_env.insert(key.clone(), value.clone());
    }

    (non_login_command, filtered_env)
}

#[cfg(all(test, unix))]
#[path = "runtimes/mod_tests.rs"]
mod tests;
