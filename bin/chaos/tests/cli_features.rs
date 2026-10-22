use anyhow::Result;
use assert_cmd::Command;
use tempfile::TempDir;

fn command(home: &TempDir) -> Command {
    chaos_kern::test_support::bootstrap_sqlite(home.path())
        .unwrap_or_else(|err| panic!("bootstrap test home: {err}"));
    // Test this feature selection, not a separately installed/exported TUI binary.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_chaos"));
    cmd.env("CHAOS_HOME", home.path())
        .env_remove("CHAOS_STORAGE_URL")
        .env_remove("CHAOS_BEARER_TOKEN")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .timeout(std::time::Duration::from_secs(10));
    cmd
}

fn stdout(home: &TempDir, args: &[&str]) -> Result<String> {
    let output = command(home).args(args).assert().success();
    Ok(String::from_utf8(output.get_output().stdout.clone())?)
}

#[test]
fn cli_feature_surface_suite() -> Result<()> {
    let home = TempDir::new()?;
    let help = stdout(&home, &["--help"])?;
    assert_eq!(
        help.contains("Expose Chaos as an ACP agent over stdio"),
        cfg!(feature = "acp")
    );
    for text in [
        "--no-alt-screen",
        "Resume a previous interactive session",
        "Fork a previous interactive session",
        "[PROMPT]",
        "[PROMPT]\n",
    ] {
        assert_eq!(help.contains(text), cfg!(feature = "tui"), "{text}");
    }
    for args in [
        &["--version"][..],
        &["serve", "--help"][..],
        &["mcp", "list", "--help"][..],
        &["mcp", "get", "--help"][..],
        &["mcp", "add", "--help"][..],
        &["mcp", "remove", "--help"][..],
        &["mcp", "login", "--help"][..],
        &["mcp", "logout", "--help"][..],
        &["taskd", "--help"][..],
        &["exec", "--help"][..],
    ] {
        stdout(&home, args)?;
    }

    assert!(stdout(&home, &["completion", "bash"])?.contains("__complete_word__"));

    let acp_completion = stdout(
        &home,
        &["__complete_word__", "--shell", "bash", "--line", "chaos ac"],
    )?;
    assert_eq!(
        acp_completion.lines().any(|line| line == "acp"),
        cfg!(feature = "acp"),
        "{acp_completion}"
    );
    #[cfg(feature = "acp")]
    assert!(stdout(&home, &["acp", "--help"])?.contains("ACP agent"));
    #[cfg(not(any(feature = "acp", feature = "tui")))]
    command(&home).arg("acp").assert().failure();

    let completion = stdout(
        &home,
        &[
            "__complete_word__",
            "--shell",
            "bash",
            "--line",
            "chaos --no",
        ],
    )?;
    assert_eq!(
        completion.contains("--no-alt-screen"),
        cfg!(feature = "tui")
    );

    #[cfg(not(feature = "tui"))]
    {
        use predicates::str::contains;
        command(&home)
            .assert()
            .failure()
            .stderr(contains("this build has no TUI"));
        command(&home)
            .arg("serve")
            .assert()
            .failure()
            .stderr(contains("bearer token is required"));
    }
    Ok(())
}

#[test]
fn mcp_daemon_feature_surface_suite() -> Result<()> {
    let home = TempDir::new()?;
    let mcp_help = stdout(&home, &["mcp", "--help"])?;
    assert_eq!(
        mcp_help.contains("Start Chaos as an MCP server"),
        cfg!(feature = "mcpd")
    );
    let serve_help = stdout(&home, &["serve", "--help"])?;
    for flag in ["--mcp", "--mcp-allowed-origin"] {
        assert_eq!(serve_help.contains(flag), cfg!(feature = "mcpd"), "{flag}");
    }
    for (line, candidate) in [
        ("chaos mcp s", "serve"),
        ("chaos serve --mcp", "--mcp"),
        ("chaos serve --mcp", "--mcp-allowed-origin"),
    ] {
        let completion = stdout(
            &home,
            &["__complete_word__", "--shell", "bash", "--line", line],
        )?;
        assert_eq!(
            completion.lines().any(|line| line == candidate),
            cfg!(feature = "mcpd"),
            "{candidate}: {completion}"
        );
    }
    #[cfg(feature = "mcpd")]
    assert!(stdout(&home, &["mcp", "serve", "--help"])?.contains("MCP server"));
    #[cfg(not(feature = "mcpd"))]
    {
        command(&home).args(["mcp", "serve"]).assert().failure();
        command(&home).args(["serve", "--mcp"]).assert().failure();
        command(&home)
            .args(["serve", "--mcp-allowed-origin", "https://example.com"])
            .assert()
            .failure();
        #[cfg(not(feature = "tui"))]
        {
            let error = command(&home).assert().failure();
            assert!(!String::from_utf8_lossy(&error.get_output().stderr).contains("mcp serve"));
        }
    }
    Ok(())
}

#[test]
fn clamp_bridge_entrypoint_is_independent_of_mcpd() -> Result<()> {
    let home = TempDir::new()?;
    // Reaching bridge-specific configuration validation proves this is the
    // real clamp entrypoint, even when the MCP daemon feature is disabled.
    command(&home)
        .arg("clamp-session-bridge")
        .env_remove("CHAOS_CLAMP_MCP_SOCKET")
        .env_remove("CHAOS_CLAMP_MCP_TOKEN")
        .assert()
        .failure()
        .stdout(predicates::str::is_empty())
        .stderr(predicates::str::contains("missing CHAOS_CLAMP_MCP_SOCKET"));
    Ok(())
}
