use super::*;
use pretty_assertions::assert_eq;
use std::ffi::OsString;
use std::sync::{LazyLock, Mutex};
use tempfile::TempDir;

static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[tokio::test]
async fn lib_suite() {
    mcp_server_defaults_analytics_to_enabled();
    is_chaos_self_reference_matches_exact_path_with_mcp_arg();
    is_chaos_self_reference_matches_basename_when_path_unresolved();
    is_chaos_self_reference_matches_resolved_path_on_path();
    is_chaos_self_reference_ignores_different_chaos_on_path();
    is_chaos_self_reference_ignores_chaos_without_mcp_arg();
    is_chaos_self_reference_ignores_unrelated_binaries();
    is_chaos_self_reference_ignores_streamable_http();
    guard_against_recursive_mcpd_allows_first_nested_level();
    guard_against_recursive_mcpd_refuses_beyond_max_depth();
}

fn mcp_server_defaults_analytics_to_enabled() {
    assert_eq!(DEFAULT_ANALYTICS_ENABLED, true);
}

fn stdio_server(command: &str, args: &[&str]) -> McpServerConfig {
    McpServerConfig {
        transport: McpServerTransportConfig::Stdio {
            command: command.to_string(),
            args: args.iter().map(|s| (*s).to_string()).collect(),
            env: None,
            env_vars: Vec::new(),
            cwd: None,
        },
        enabled: true,
        required: false,
        disabled_reason: None,
        startup_timeout_sec: None,
        tool_timeout_sec: None,
        enabled_tools: None,
        disabled_tools: None,
        scopes: None,
        oauth_resource: None,
        r#type: None,
        oauth: None,
    }
}

fn is_chaos_self_reference_matches_exact_path_with_mcp_arg() {
    let exe = PathBuf::from("/usr/local/bin/chaos");
    let entry = stdio_server("/usr/local/bin/chaos", &["mcp", "serve"]);
    assert!(is_chaos_self_reference(
        &entry,
        Some(exe.as_path()),
        Some("chaos"),
    ));
}

fn is_chaos_self_reference_matches_basename_when_path_unresolved() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let saved_path = std::env::var_os("PATH");
    unsafe {
        std::env::set_var("PATH", OsString::new());
    }
    let exe = PathBuf::from("/does/not/exist/chaos");
    // Bare `chaos` with no PATH resolution available — basename fallback hits.
    let entry = stdio_server("chaos", &["mcp", "serve"]);
    let is_self = is_chaos_self_reference(&entry, Some(exe.as_path()), Some("chaos"));
    unsafe {
        match saved_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }
    assert!(is_self);
}

fn is_chaos_self_reference_matches_resolved_path_on_path() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let saved_path = std::env::var_os("PATH");
    let temp = TempDir::new().unwrap();
    let self_dir = temp.path().join("self");
    std::fs::create_dir_all(&self_dir).unwrap();
    let self_exe = self_dir.join("chaos");
    std::fs::write(&self_exe, b"#!/bin/sh\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&self_exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    unsafe {
        std::env::set_var("PATH", &self_dir);
    }

    let entry = stdio_server("chaos", &["mcp", "serve"]);
    let is_self = is_chaos_self_reference(&entry, Some(self_exe.as_path()), Some("chaos"));

    unsafe {
        match saved_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }
    assert!(is_self);
}

fn is_chaos_self_reference_ignores_different_chaos_on_path() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let saved_path = std::env::var_os("PATH");
    let temp = TempDir::new().unwrap();
    let self_dir = temp.path().join("self");
    let other_dir = temp.path().join("other");
    std::fs::create_dir_all(&self_dir).unwrap();
    std::fs::create_dir_all(&other_dir).unwrap();

    let self_exe = self_dir.join("chaos");
    let other_exe = other_dir.join("chaos");
    std::fs::write(&self_exe, b"#!/bin/sh\n").unwrap();
    std::fs::write(&other_exe, b"#!/bin/sh\n").unwrap();
    unsafe {
        std::env::set_var("PATH", &other_dir);
    }

    let entry = stdio_server("chaos", &["mcp", "serve"]);
    let is_self = is_chaos_self_reference(&entry, Some(self_exe.as_path()), Some("chaos"));

    unsafe {
        match saved_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }
    assert!(!is_self);
}

fn is_chaos_self_reference_ignores_chaos_without_mcp_arg() {
    let exe = PathBuf::from("/usr/local/bin/chaos");
    // `chaos exec foo` is a different role — do not strip.
    let entry = stdio_server("/usr/local/bin/chaos", &["exec", "foo"]);
    assert!(!is_chaos_self_reference(
        &entry,
        Some(exe.as_path()),
        Some("chaos"),
    ));
}

fn is_chaos_self_reference_ignores_unrelated_binaries() {
    let exe = PathBuf::from("/usr/local/bin/chaos");
    let entry = stdio_server("dictator", &["mcp"]);
    assert!(!is_chaos_self_reference(
        &entry,
        Some(exe.as_path()),
        Some("chaos"),
    ));
}

fn is_chaos_self_reference_ignores_streamable_http() {
    let exe = PathBuf::from("/usr/local/bin/chaos");
    let entry = McpServerConfig {
        transport: McpServerTransportConfig::StreamableHttp {
            url: "http://localhost:3011/mcp".to_string(),
            bearer_token: None,
            bearer_token_env_var: None,
            http_headers: None,
            env_http_headers: None,
        },
        ..stdio_server("unused", &[])
    };
    assert!(!is_chaos_self_reference(
        &entry,
        Some(exe.as_path()),
        Some("chaos"),
    ));
}

fn guard_against_recursive_mcpd_allows_first_nested_level() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let saved = std::env::var(CHAOS_MCPD_DEPTH_ENV).ok();
    unsafe {
        std::env::set_var(CHAOS_MCPD_DEPTH_ENV, CHAOS_MCPD_MAX_DEPTH.to_string());
    }
    let result = guard_against_recursive_mcpd();
    assert!(result.is_ok(), "expected first nested server to be allowed");
    assert_eq!(
        std::env::var(CHAOS_MCPD_DEPTH_ENV),
        Ok((CHAOS_MCPD_MAX_DEPTH + 1).to_string())
    );
    unsafe {
        match saved {
            Some(v) => std::env::set_var(CHAOS_MCPD_DEPTH_ENV, v),
            None => std::env::remove_var(CHAOS_MCPD_DEPTH_ENV),
        }
    }
}

fn guard_against_recursive_mcpd_refuses_beyond_max_depth() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let saved = std::env::var(CHAOS_MCPD_DEPTH_ENV).ok();
    unsafe {
        std::env::set_var(CHAOS_MCPD_DEPTH_ENV, (CHAOS_MCPD_MAX_DEPTH + 1).to_string());
    }
    let result = guard_against_recursive_mcpd();
    assert!(result.is_err(), "expected recursion guard to trip");
    assert_eq!(result.err().unwrap().kind(), ErrorKind::WouldBlock);
    unsafe {
        match saved {
            Some(v) => std::env::set_var(CHAOS_MCPD_DEPTH_ENV, v),
            None => std::env::remove_var(CHAOS_MCPD_DEPTH_ENV),
        }
    }
}
