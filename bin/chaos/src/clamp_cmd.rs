//! Machine-readable lifecycle boundary for first-party clamp transports.

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use clap::Args;
use clap::Subcommand;
use serde_json::json;

#[derive(Debug, Args)]
pub(crate) struct ClampCli {
    #[command(subcommand)]
    command: ClampCommand,
}

#[derive(Debug, Subcommand)]
enum ClampCommand {
    /// Inspect the Google Antigravity clamp transport.
    Antigravity(AntigravityCli),
}

#[derive(Debug, Args)]
struct AntigravityCli {
    #[command(subcommand)]
    command: AntigravityCommand,
}

#[derive(Debug, Subcommand)]
enum AntigravityCommand {
    /// Report transport availability, version, authentication state, and capabilities.
    Status {
        /// Emit one JSON object.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Run the official Antigravity browser sign-in ceremony.
    Connect,
    /// Remove Antigravity credentials and Chaos provider-conversation mappings.
    Disconnect {
        /// Emit one JSON object.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

impl ClampCli {
    pub(crate) async fn run(self) -> anyhow::Result<()> {
        match self.command {
            ClampCommand::Antigravity(command) => run_antigravity(command).await,
        }
    }
}

async fn run_antigravity(cli: AntigravityCli) -> anyhow::Result<()> {
    match cli.command {
        AntigravityCommand::Status { json } => {
            let status = antigravity_status();
            if json {
                println!("{}", serde_json::to_string(&status)?);
            } else {
                println!("Antigravity clamp");
                println!(
                    "  CLI: {}",
                    if status["available"] == true {
                        "available"
                    } else {
                        "unavailable"
                    }
                );
                println!(
                    "  Version: {}",
                    status["version"].as_str().unwrap_or("unknown")
                );
                println!(
                    "  Authentication: {}",
                    status["authentication_state"].as_str().unwrap_or("unknown")
                );
                println!("  Tool authority: chaos-session-bridge");
            }
            Ok(())
        }
        AntigravityCommand::Connect => antigravity_connect().await,
        AntigravityCommand::Disconnect { json } => {
            antigravity_disconnect()?;
            if json {
                println!(
                    "{}",
                    json!({
                        "backend": "antigravity",
                        "authentication_state": "none",
                    })
                );
            } else {
                println!("Disconnected the Antigravity clamp account.");
            }
            Ok(())
        }
    }
}

fn antigravity_status() -> serde_json::Value {
    let cli_path = antigravity_cli_path();
    let version = cli_path.as_deref().and_then(antigravity_version);
    let credential_present = antigravity_home()
        .map(|home| antigravity_token_path(&home))
        .and_then(|path| std::fs::metadata(path).ok())
        .is_some_and(|metadata| metadata.is_file() && metadata.len() > 0);

    json!({
        "backend": "antigravity",
        "available": cli_path.is_some(),
        "version": version,
        "authentication_state": if credential_present {
            "credential-present"
        } else {
            "none"
        },
        "tool_authority": "chaos-session-bridge",
        "capabilities": {
            "exec": true,
            "chaos_resume": true,
            "provider_conversation_resume": true,
            "connect": true,
            "disconnect": true,
            "incremental_output": false,
            "chaos_tool_bridge": true,
        }
    })
}

async fn antigravity_connect() -> anyhow::Result<()> {
    let cli_path = antigravity_cli_path()
        .context("Antigravity CLI not found; set CHAOS_AGY_PATH or install agy in PATH")?;
    let home = antigravity_home()
        .context("Antigravity home unavailable; set CHAOS_AGY_HOME to a dedicated directory")?;
    std::fs::create_dir_all(&home)
        .with_context(|| format!("create Antigravity home {}", home.display()))?;
    #[cfg(unix)]
    std::fs::set_permissions(&home, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;

    let status = tokio::process::Command::new(cli_path)
        .arg("models")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env_remove("GEMINI_API_KEY")
        .env_remove("GOOGLE_API_KEY")
        .status()
        .await
        .context("run Antigravity sign-in ceremony")?;
    if !status.success() {
        anyhow::bail!("Antigravity sign-in was not completed");
    }
    if !antigravity_token_path(&home).is_file() {
        anyhow::bail!("Antigravity exited successfully but no credential state was created");
    }
    println!("Antigravity account connected.");
    Ok(())
}

fn antigravity_disconnect() -> anyhow::Result<()> {
    let home = antigravity_home()
        .context("Antigravity home unavailable; set CHAOS_AGY_HOME to a dedicated directory")?;
    let token_path = antigravity_token_path(&home);
    match std::fs::remove_file(&token_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("remove Antigravity token {}", token_path.display()));
        }
    }
    let conversation_dir = home.join(".chaos-conversations");
    match std::fs::remove_dir_all(&conversation_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "remove Antigravity conversation mappings {}",
                    conversation_dir.display()
                )
            });
        }
    }
    Ok(())
}

fn antigravity_cli_path() -> Option<PathBuf> {
    match std::env::var_os("CHAOS_AGY_PATH") {
        Some(path) => {
            let path = PathBuf::from(path);
            path.is_file().then_some(path)
        }
        None => which::which("agy").ok(),
    }
}

fn antigravity_home() -> Option<PathBuf> {
    std::env::var_os("CHAOS_AGY_HOME").map(PathBuf::from)
}

fn antigravity_token_path(home: &Path) -> PathBuf {
    home.join(".gemini")
        .join("antigravity-cli")
        .join("antigravity-oauth-token")
}

fn antigravity_version(cli_path: &Path) -> Option<String> {
    let output = std::process::Command::new(cli_path)
        .arg("--version")
        .env_remove("GEMINI_API_KEY")
        .env_remove("GOOGLE_API_KEY")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!version.is_empty()).then_some(version)
}
