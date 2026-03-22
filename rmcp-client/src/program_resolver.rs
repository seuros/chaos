//! Program resolution for MCP server execution.
//!
//! On Unix, the OS handles PATH resolution and script execution natively
//! through the kernel's shebang (`#!`) mechanism, so this function simply
//! returns the program name unchanged.

use std::collections::HashMap;
use std::ffi::OsString;

/// Resolves a program to its executable path.
///
/// On Unix the OS handles script execution natively, so the program is
/// returned unchanged.
pub fn resolve(program: OsString, _env: &HashMap<String, String>) -> std::io::Result<OsString> {
    Ok(program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::create_env_for_mcp_server;
    use anyhow::Result;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;
    use tokio::process::Command;

    /// Verifies the OS handles script execution without file extensions.
    #[tokio::test]
    async fn test_executes_script_without_extension() -> Result<()> {
        let env = TestExecutableEnv::new()?;
        let mut cmd = Command::new(&env.program_name);
        cmd.envs(&env.mcp_env);

        let output = cmd.output().await;
        assert!(output.is_ok(), "Unix should execute scripts directly");
        Ok(())
    }

    /// Verifies program resolution enables successful execution.
    #[tokio::test]
    async fn test_resolved_program_executes_successfully() -> Result<()> {
        let env = TestExecutableEnv::new()?;
        let program = OsString::from(&env.program_name);

        let resolved = resolve(program, &env.mcp_env)?;

        let mut cmd = Command::new(resolved);
        cmd.envs(&env.mcp_env);
        let output = cmd.output().await;

        assert!(
            output.is_ok(),
            "Resolved program should execute successfully"
        );
        Ok(())
    }

    // Test fixture for creating temporary executables in a controlled environment.
    struct TestExecutableEnv {
        // Held to prevent the temporary directory from being deleted.
        _temp_dir: TempDir,
        program_name: String,
        mcp_env: HashMap<String, String>,
    }

    impl TestExecutableEnv {
        const TEST_PROGRAM: &'static str = "test_mcp_server";

        fn new() -> Result<Self> {
            let temp_dir = TempDir::new()?;
            let dir_path = temp_dir.path();

            Self::create_executable(dir_path)?;

            // Build a clean environment with the temp dir in the PATH.
            let mut extra_env = HashMap::new();
            extra_env.insert("PATH".to_string(), Self::build_path(dir_path));

            let mcp_env = create_env_for_mcp_server(Some(extra_env), &[]);

            Ok(Self {
                _temp_dir: temp_dir,
                program_name: Self::TEST_PROGRAM.to_string(),
                mcp_env,
            })
        }

        fn create_executable(dir: &Path) -> Result<()> {
            let file = dir.join(Self::TEST_PROGRAM);
            fs::write(&file, "#!/bin/sh\nexit 0")?;
            Self::set_executable(&file)?;
            Ok(())
        }

        fn set_executable(path: &Path) -> Result<()> {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(path)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(path, perms)?;
            Ok(())
        }

        fn build_path(dir: &Path) -> String {
            let current = std::env::var("PATH").unwrap_or_default();
            format!("{}:{current}", dir.to_string_lossy())
        }
    }
}
