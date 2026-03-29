use super::*;
use crate::config::types::ShellEnvironmentPolicyInherit;
use std::collections::HashMap;

fn make_vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn test_core_inherit_defaults_keep_sensitive_vars() {
    let vars = make_vars(&[
        ("PATH", "/usr/bin"),
        ("HOME", "/home/user"),
        ("API_KEY", "secret"),
        ("SECRET_TOKEN", "t"),
    ]);

    let policy = ShellEnvironmentPolicy::default(); // inherit All, default excludes ignored
    let process_id = ProcessId::new();
    let result = populate_env(vars, &policy, Some(process_id));

    let mut expected: HashMap<String, String> = HashMap::from([
        ("PATH".to_string(), "/usr/bin".to_string()),
        ("HOME".to_string(), "/home/user".to_string()),
        ("API_KEY".to_string(), "secret".to_string()),
        ("SECRET_TOKEN".to_string(), "t".to_string()),
    ]);
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), process_id.to_string());

    assert_eq!(result, expected);
}

#[test]
fn test_core_inherit_with_default_excludes_enabled() {
    let vars = make_vars(&[
        ("PATH", "/usr/bin"),
        ("HOME", "/home/user"),
        ("API_KEY", "secret"),
        ("SECRET_TOKEN", "t"),
    ]);

    let policy = ShellEnvironmentPolicy {
        ignore_default_excludes: false, // apply KEY/SECRET/TOKEN filter
        ..Default::default()
    };
    let process_id = ProcessId::new();
    let result = populate_env(vars, &policy, Some(process_id));

    let mut expected: HashMap<String, String> = HashMap::from([
        ("PATH".to_string(), "/usr/bin".to_string()),
        ("HOME".to_string(), "/home/user".to_string()),
    ]);
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), process_id.to_string());

    assert_eq!(result, expected);
}

#[test]
fn test_include_only() {
    let vars = make_vars(&[("PATH", "/usr/bin"), ("FOO", "bar")]);

    let policy = ShellEnvironmentPolicy {
        // skip default excludes so nothing is removed prematurely
        ignore_default_excludes: true,
        include_only: vec![EnvironmentVariablePattern::new_case_insensitive("*PATH")],
        ..Default::default()
    };

    let process_id = ProcessId::new();
    let result = populate_env(vars, &policy, Some(process_id));

    let mut expected: HashMap<String, String> =
        HashMap::from([("PATH".to_string(), "/usr/bin".to_string())]);
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), process_id.to_string());

    assert_eq!(result, expected);
}

#[test]
fn test_set_overrides() {
    let vars = make_vars(&[("PATH", "/usr/bin")]);

    let mut policy = ShellEnvironmentPolicy {
        ignore_default_excludes: true,
        ..Default::default()
    };
    policy.r#set.insert("NEW_VAR".to_string(), "42".to_string());

    let process_id = ProcessId::new();
    let result = populate_env(vars, &policy, Some(process_id));

    let mut expected: HashMap<String, String> = HashMap::from([
        ("PATH".to_string(), "/usr/bin".to_string()),
        ("NEW_VAR".to_string(), "42".to_string()),
    ]);
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), process_id.to_string());

    assert_eq!(result, expected);
}

#[test]
fn populate_env_inserts_process_id() {
    let vars = make_vars(&[("PATH", "/usr/bin")]);
    let policy = ShellEnvironmentPolicy::default();
    let process_id = ProcessId::new();
    let result = populate_env(vars, &policy, Some(process_id));

    let mut expected: HashMap<String, String> =
        HashMap::from([("PATH".to_string(), "/usr/bin".to_string())]);
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), process_id.to_string());

    assert_eq!(result, expected);
}

#[test]
fn populate_env_omits_process_id_when_missing() {
    let vars = make_vars(&[("PATH", "/usr/bin")]);
    let policy = ShellEnvironmentPolicy::default();
    let result = populate_env(vars, &policy, None);

    let expected: HashMap<String, String> =
        HashMap::from([("PATH".to_string(), "/usr/bin".to_string())]);

    assert_eq!(result, expected);
}

#[test]
fn test_inherit_all() {
    let vars = make_vars(&[("PATH", "/usr/bin"), ("FOO", "bar")]);

    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::All,
        ignore_default_excludes: true, // keep everything
        ..Default::default()
    };

    let process_id = ProcessId::new();
    let result = populate_env(vars.clone(), &policy, Some(process_id));
    let mut expected: HashMap<String, String> = vars.into_iter().collect();
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), process_id.to_string());
    assert_eq!(result, expected);
}

#[test]
fn test_inherit_all_with_default_excludes() {
    let vars = make_vars(&[("PATH", "/usr/bin"), ("API_KEY", "secret")]);

    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::All,
        ignore_default_excludes: false,
        ..Default::default()
    };

    let process_id = ProcessId::new();
    let result = populate_env(vars, &policy, Some(process_id));
    let mut expected: HashMap<String, String> =
        HashMap::from([("PATH".to_string(), "/usr/bin".to_string())]);
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), process_id.to_string());
    assert_eq!(result, expected);
}

#[test]
fn test_inherit_none() {
    let vars = make_vars(&[("PATH", "/usr/bin"), ("HOME", "/home")]);

    let mut policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::None,
        ignore_default_excludes: true,
        ..Default::default()
    };
    policy
        .r#set
        .insert("ONLY_VAR".to_string(), "yes".to_string());

    let process_id = ProcessId::new();
    let result = populate_env(vars, &policy, Some(process_id));
    let mut expected: HashMap<String, String> =
        HashMap::from([("ONLY_VAR".to_string(), "yes".to_string())]);
    expected.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), process_id.to_string());
    assert_eq!(result, expected);
}

#[test]
fn populate_env_strips_reserved_sandbox_markers() {
    let vars = make_vars(&[
        ("PATH", "/usr/bin"),
        (crate::spawn::CODEX_SANDBOX_ENV_VAR, "seatbelt"),
        (crate::spawn::CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR, "1"),
    ]);

    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::All,
        ignore_default_excludes: true,
        ..Default::default()
    };

    let result = populate_env(vars, &policy, None);

    let expected: HashMap<String, String> =
        HashMap::from([("PATH".to_string(), "/usr/bin".to_string())]);

    assert_eq!(result, expected);
}
