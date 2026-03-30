use crate::config::types::EnvironmentVariablePattern;
use crate::config::types::ShellEnvironmentPolicy;
use crate::config::types::ShellEnvironmentPolicyInherit;
use crate::spawn::CHAOS_SANDBOX_ENV_VAR;
use crate::spawn::CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR;
use chaos_ipc::ProcessId;
use std::collections::HashMap;
use std::collections::HashSet;

pub const CHAOS_THREAD_ID_ENV_VAR: &str = "CHAOS_THREAD_ID";

/// Construct an environment map based on the rules in the specified policy. The
/// resulting map can be passed directly to `Command::envs()` after calling
/// `env_clear()` to ensure no unintended variables are leaked to the spawned
/// process.
///
/// The derivation follows the algorithm documented in the struct-level comment
/// for [`ShellEnvironmentPolicy`].
///
/// `CHAOS_THREAD_ID` is injected when a thread id is provided, even when
/// `include_only` is set.
pub fn create_env(
    policy: &ShellEnvironmentPolicy,
    process_id: Option<ProcessId>,
) -> HashMap<String, String> {
    populate_env(std::env::vars(), policy, process_id)
}

fn populate_env<I>(
    vars: I,
    policy: &ShellEnvironmentPolicy,
    process_id: Option<ProcessId>,
) -> HashMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    // Step 1 – determine the starting set of variables based on the
    // `inherit` strategy.
    let mut env_map: HashMap<String, String> = match policy.inherit {
        ShellEnvironmentPolicyInherit::All => vars.into_iter().collect(),
        ShellEnvironmentPolicyInherit::None => HashMap::new(),
        ShellEnvironmentPolicyInherit::Core => {
            const CORE_VARS: &[&str] = &[
                "HOME", "LOGNAME", "PATH", "SHELL", "USER", "USERNAME", "TMPDIR", "TEMP", "TMP",
            ];
            let allow: HashSet<&str> = CORE_VARS.iter().copied().collect();
            let is_core_var = |name: &str| allow.contains(name);
            vars.into_iter().filter(|(k, _)| is_core_var(k)).collect()
        }
    };

    // Internal helper – does `name` match **any** pattern in `patterns`?
    let matches_any = |name: &str, patterns: &[EnvironmentVariablePattern]| -> bool {
        patterns.iter().any(|pattern| pattern.matches(name))
    };

    // Step 2 – Apply the default exclude if not disabled.
    if !policy.ignore_default_excludes {
        let default_excludes = vec![
            EnvironmentVariablePattern::new_case_insensitive("*KEY*"),
            EnvironmentVariablePattern::new_case_insensitive("*SECRET*"),
            EnvironmentVariablePattern::new_case_insensitive("*TOKEN*"),
        ];
        env_map.retain(|k, _| !matches_any(k, &default_excludes));
    }

    // Step 3 – Apply custom excludes.
    if !policy.exclude.is_empty() {
        env_map.retain(|k, _| !matches_any(k, &policy.exclude));
    }

    // Step 4 – Apply user-provided overrides.
    for (key, val) in &policy.r#set {
        env_map.insert(key.clone(), val.clone());
    }

    // Step 5 – If include_only is non-empty, keep *only* the matching vars.
    if !policy.include_only.is_empty() {
        env_map.retain(|k, _| matches_any(k, &policy.include_only));
    }

    // Step 6 – Strip reserved sandbox markers inherited from the parent
    // process. These are runtime implementation details and must be re-added
    // only by the actual spawn/sandbox path that applies to this child.
    env_map.remove(CHAOS_SANDBOX_ENV_VAR);
    env_map.remove(CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR);

    // Step 7 – Populate the thread ID environment variable when provided.
    if let Some(process_id) = process_id {
        env_map.insert(CHAOS_THREAD_ID_ENV_VAR.to_string(), process_id.to_string());
    }

    env_map
}

#[cfg(test)]
#[path = "exec_env_tests.rs"]
mod tests;
