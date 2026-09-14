use anyhow::Result;
use assert_cmd::Command;
use tempfile::TempDir;

fn command(home: &TempDir) -> Command {
    // Test this feature selection, not a separately installed/exported TUI binary.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_chaos"));
    cmd.env("CHAOS_HOME", home.path())
        .env_remove("CHAOS_STORAGE_URL")
        .env_remove("CHAOS_SQLITE_HOME")
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
        &["mcp", "serve", "--help"][..],
        &["taskd", "--help"][..],
        &["exec", "--help"][..],
    ] {
        stdout(&home, args)?;
    }

    assert!(stdout(&home, &["completion", "bash"])?.contains("__complete_word__"));

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
