//! Public-API tests for `chaos-getopt` — the CLI surface the rest of
//! the workspace leans on.
//!
//! These aren't clap-parser stress tests; they pin the two conversions
//! the rest of the code takes for granted: `SandboxModeCliArg` → policy
//! enum, and the environment-rendering helper that keeps secrets from
//! leaking into logs.

use std::collections::HashMap;

use chaos_getopt::SandboxModeCliArg;
use chaos_getopt::format_env_display::format_env_display;
use chaos_ipc::config_types::SandboxMode;
use pretty_assertions::assert_eq;

#[test]
fn sandbox_cli_variants_map_to_the_matching_policy_modes() {
    // Every CLI arm must land on its sibling policy variant — drift
    // here would silently change runtime sandboxing.
    assert_eq!(SandboxMode::ReadOnly, SandboxModeCliArg::ReadOnly.into());
    assert_eq!(
        SandboxMode::WorkspaceWrite,
        SandboxModeCliArg::WorkspaceWrite.into()
    );
    assert_eq!(
        SandboxMode::RootAccess,
        SandboxModeCliArg::RootAccess.into()
    );
}

#[test]
fn env_display_sorts_pairs_masks_values_and_falls_back_to_dash() {
    // No map, no vars → sentinel dash so tooling doesn't render an
    // empty field.
    assert_eq!(format_env_display(None, &[]), "-");
    let empty_map = HashMap::new();
    assert_eq!(format_env_display(Some(&empty_map), &[]), "-");

    // Env pairs render alphabetically with every value replaced by
    // stars. Insertion order must not leak into logs.
    let mut env = HashMap::new();
    env.insert("B".to_string(), "two".to_string());
    env.insert("A".to_string(), "one".to_string());
    assert_eq!(format_env_display(Some(&env), &[]), "A=*****, B=*****");

    // Inherited env var names also get star-masked and joined after
    // the map pairs.
    let vars = vec!["TOKEN".to_string(), "PATH".to_string()];
    assert_eq!(format_env_display(None, &vars), "TOKEN=*****, PATH=*****");

    // Combined rendering preserves both halves.
    let mut home_env = HashMap::new();
    home_env.insert("HOME".to_string(), "/tmp".to_string());
    let single = vec!["TOKEN".to_string()];
    assert_eq!(
        format_env_display(Some(&home_env), &single),
        "HOME=*****, TOKEN=*****"
    );
}
