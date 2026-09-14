use super::*;
use assert_matches::assert_matches;
use chaos_ipc::ProcessId;
use chaos_ipc::protocol::TokenUsage;
use pretty_assertions::assert_eq;
use std::ffi::OsStr;

fn try_parse_cli<'v>(args: &[&'v str]) -> Result<MultitoolCli, usage::Error<'static, 'v>> {
    let argv = args.iter().map(|arg| OsStr::new(*arg)).collect::<Vec<_>>();
    let mut cli = MultitoolCli::try_parse_from(&argv)?;
    cli.normalize();
    Ok(cli)
}

fn finalize_resume_from_args(args: &[&str]) -> TuiCli {
    let cli = try_parse_cli(args).expect("parse");
    let MultitoolCli {
        debug: _,
        interactive,
        config_overrides: root_overrides,
        subcommand,
        provider: _,
    } = cli;

    let Subcommand::Resume(ResumeCommand {
        session_id,
        last,
        all,
        config_overrides: resume_cli,
    }) = subcommand.expect("resume present")
    else {
        unreachable!()
    };

    finalize_resume_interactive(
        interactive,
        root_overrides,
        session_id,
        last,
        all,
        resume_cli,
    )
}

fn finalize_fork_from_args(args: &[&str]) -> TuiCli {
    let cli = try_parse_cli(args).expect("parse");
    let MultitoolCli {
        debug: _,
        interactive,
        config_overrides: root_overrides,
        subcommand,
        provider: _,
    } = cli;

    let Subcommand::Fork(ForkCommand {
        session_id,
        last,
        all,
        config_overrides: fork_cli,
    }) = subcommand.expect("fork present")
    else {
        unreachable!()
    };

    finalize_fork_interactive(interactive, root_overrides, session_id, last, all, fork_cli)
}

#[test]
fn cli_parser_and_exit_format_suite() {
    exec_resume_cli_parses_positionals_and_subcommand_flags();
    auto_exec_flags_do_not_leak_to_unrelated_subcommands();
    format_exit_messages_handles_zero_usage_resume_color_and_thread_names();
    resume_and_fork_picker_logic_cover_default_last_session_and_all_modes();
    resume_merges_subcommand_scoped_flags_with_highest_precedence();
    debug_flag_is_global_and_defaults_false();
    mcp_add_transport_shapes_and_constraints_are_preserved();
    completion_shells_and_global_provider_are_preserved();
    global_config_order_and_duplicate_scalar_rejection_are_preserved();
}

fn exec_resume_cli_parses_positionals_and_subcommand_flags() {
    let cli = try_parse_cli(&["chaos", "exec", "--json", "resume", "--last", "2+2"])
        .expect("parse should succeed");
    let Some(Subcommand::Exec(exec)) = cli.subcommand else {
        panic!("expected exec subcommand");
    };
    let Some(chaos_fork::Command::Resume(args)) = exec.command else {
        panic!("expected exec resume");
    };
    assert!(args.last);
    assert_eq!(args.session_id, None);
    assert_eq!(args.prompt.as_deref(), Some("2+2"));

    let cli = try_parse_cli(&[
        "chaos",
        "exec",
        "resume",
        "session-123",
        "-o",
        "/tmp/resume-output.md",
        "re-review",
    ])
    .expect("parse should succeed");
    let Some(Subcommand::Exec(exec)) = cli.subcommand else {
        panic!("expected exec subcommand");
    };
    let Some(chaos_fork::Command::Resume(args)) = exec.command else {
        panic!("expected exec resume");
    };
    assert_eq!(
        exec.last_message_file,
        Some(std::path::PathBuf::from("/tmp/resume-output.md"))
    );
    assert_eq!(args.session_id.as_deref(), Some("session-123"));
    assert_eq!(args.prompt.as_deref(), Some("re-review"));

    let cli = try_parse_cli(&[
        "chaos",
        "exec",
        "resume",
        "--last",
        "--headless",
        "continue",
    ])
    .expect("parse should succeed");
    let Some(Subcommand::Exec(exec)) = cli.subcommand else {
        panic!("expected exec subcommand");
    };
    let Some(chaos_fork::Command::Resume(args)) = exec.command else {
        panic!("expected exec resume");
    };
    assert!(exec.auto_exec.headless);
    assert!(args.last);
    assert_eq!(args.prompt.as_deref(), Some("continue"));
}

fn auto_exec_flags_do_not_leak_to_unrelated_subcommands() {
    for args in [
        &["chaos", "accounts", "--headless"][..],
        &["chaos", "accounts", "--full-auto"][..],
        &["chaos", "completion", "--headless"][..],
        &["chaos", "completion", "--full-auto"][..],
    ] {
        let err = try_parse_cli(args).expect_err("parse should fail");
        assert!(matches!(
            err,
            usage::Error::UnknownFlag { .. } | usage::Error::UnexpectedArg { .. }
        ));
    }
}

fn sample_exit_info(conversation_id: Option<&str>, process_name: Option<&str>) -> AppExitInfo {
    let token_usage = TokenUsage {
        output_tokens: 2,
        total_tokens: 2,
        ..Default::default()
    };
    AppExitInfo {
        token_usage,
        process_id: conversation_id
            .map(ProcessId::from_string)
            .map(Result::unwrap),
        process_name: process_name.map(str::to_string),
        exit_reason: ExitReason::UserRequested,
    }
}

fn format_exit_messages_handles_zero_usage_resume_color_and_thread_names() {
    let exit_info = AppExitInfo {
        token_usage: TokenUsage::default(),
        process_id: None,
        process_name: None,
        exit_reason: ExitReason::UserRequested,
    };
    let lines = format_exit_messages(exit_info, false);
    assert!(lines.is_empty());

    let exit_info = sample_exit_info(Some("123e4567-e89b-12d3-a456-426614174000"), None);
    let lines = format_exit_messages(exit_info, false);
    assert_eq!(
        lines,
        vec![
            "Token usage: total=2 input=0 output=2".to_string(),
            "To continue this session, run chaos resume 123e4567-e89b-12d3-a456-426614174000"
                .to_string(),
        ]
    );

    let exit_info = sample_exit_info(Some("123e4567-e89b-12d3-a456-426614174000"), None);
    let lines = format_exit_messages(exit_info, true);
    assert_eq!(lines.len(), 2);
    assert!(lines[1].contains("\u{1b}[36m"));

    let exit_info = sample_exit_info(
        Some("123e4567-e89b-12d3-a456-426614174000"),
        Some("my-thread"),
    );
    let lines = format_exit_messages(exit_info, false);
    assert_eq!(
        lines,
        vec![
            "Token usage: total=2 input=0 output=2".to_string(),
            "To continue this session by name, run chaos resume my-thread".to_string(),
            "Or by session ID, run chaos resume 123e4567-e89b-12d3-a456-426614174000".to_string(),
        ]
    );
}

fn resume_and_fork_picker_logic_cover_default_last_session_and_all_modes() {
    let interactive = finalize_resume_from_args(["chaos", "resume"].as_ref());
    assert!(interactive.resume_picker);
    assert!(!interactive.resume_last);
    assert_eq!(interactive.resume_session_id, None);
    assert!(!interactive.resume_show_all);

    let interactive = finalize_resume_from_args(["chaos", "resume", "--last"].as_ref());
    assert!(!interactive.resume_picker);
    assert!(interactive.resume_last);
    assert_eq!(interactive.resume_session_id, None);
    assert!(!interactive.resume_show_all);

    let interactive = finalize_resume_from_args(["chaos", "resume", "1234"].as_ref());
    assert!(!interactive.resume_picker);
    assert!(!interactive.resume_last);
    assert_eq!(interactive.resume_session_id.as_deref(), Some("1234"));
    assert!(!interactive.resume_show_all);

    let interactive = finalize_resume_from_args(["chaos", "resume", "--all"].as_ref());
    assert!(interactive.resume_picker);
    assert!(interactive.resume_show_all);

    let interactive = finalize_fork_from_args(["chaos", "fork"].as_ref());
    assert!(interactive.fork_picker);
    assert!(!interactive.fork_last);
    assert_eq!(interactive.fork_session_id, None);
    assert!(!interactive.fork_show_all);

    let interactive = finalize_fork_from_args(["chaos", "fork", "--last"].as_ref());
    assert!(!interactive.fork_picker);
    assert!(interactive.fork_last);
    assert_eq!(interactive.fork_session_id, None);
    assert!(!interactive.fork_show_all);

    let interactive = finalize_fork_from_args(["chaos", "fork", "1234"].as_ref());
    assert!(!interactive.fork_picker);
    assert!(!interactive.fork_last);
    assert_eq!(interactive.fork_session_id.as_deref(), Some("1234"));
    assert!(!interactive.fork_show_all);

    let interactive = finalize_fork_from_args(["chaos", "fork", "--all"].as_ref());
    assert!(interactive.fork_picker);
    assert!(interactive.fork_show_all);
}

fn resume_merges_subcommand_scoped_flags_with_highest_precedence() {
    let interactive = finalize_resume_from_args(
        [
            "chaos",
            "resume",
            "sid",
            "--full-auto",
            "--search",
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "interactive",
            "-p",
            "my-profile",
            "-C",
            "/tmp",
        ]
        .as_ref(),
    );

    assert_eq!(interactive.config_profile.as_deref(), Some("my-profile"));
    assert_matches!(
        interactive.sandbox_mode,
        Some(chaos_getopt::SandboxModeCliArg::WorkspaceWrite)
    );
    assert_matches!(
        interactive.approval_policy,
        Some(chaos_getopt::ApprovalModeCliArg::Interactive)
    );
    assert!(interactive.auto_exec.full_auto);
    assert_eq!(
        interactive.cwd.as_deref(),
        Some(std::path::Path::new("/tmp"))
    );
    assert!(interactive.web_search);
    assert!(!interactive.resume_picker);
    assert!(!interactive.resume_last);
    assert_eq!(interactive.resume_session_id.as_deref(), Some("sid"));

    let interactive = finalize_resume_from_args(["chaos", "resume", "--headless"].as_ref());
    assert!(interactive.auto_exec.headless);
    assert!(interactive.resume_picker);
    assert!(!interactive.resume_last);
    assert_eq!(interactive.resume_session_id, None);
}

fn debug_flag_is_global_and_defaults_false() {
    for args in [
        &["chaos", "--debug"][..],
        &["chaos", "-d"][..],
        &["chaos", "--debug", "exec", "say hi"][..],
        &["chaos", "exec", "--debug", "say hi"][..],
    ] {
        let cli = try_parse_cli(args).expect("parse");
        assert!(cli.debug, "debug should be enabled for {args:?}");
    }

    let cli = try_parse_cli(&["chaos"]).expect("parse");
    assert!(!cli.debug);
}

fn mcp_add_transport_shapes_and_constraints_are_preserved() {
    let cli = try_parse_cli(&[
        "chaos",
        "mcp",
        "add",
        "local",
        "--env",
        "TOKEN=secret",
        "--",
        "node",
        "server.js",
    ])
    .expect("stdio transport should parse");
    let Some(Subcommand::Mcp(mcp)) = cli.subcommand else {
        panic!("expected mcp subcommand");
    };
    let crate::mcp_cmd::McpSubcommand::Add(add) = mcp.subcommand else {
        panic!("expected mcp add");
    };
    assert_eq!(add.name, "local");
    assert_eq!(add.transport_args.command, ["node", "server.js"]);
    assert_eq!(add.transport_args.env.len(), 1);
    assert_eq!(add.transport_args.env[0].0, "TOKEN");
    assert_eq!(add.transport_args.env[0].1, "secret");
    assert_eq!(add.transport_args.url, None);

    let cli = try_parse_cli(&[
        "chaos",
        "mcp",
        "add",
        "remote",
        "--url",
        "https://example.test/mcp",
        "--bearer-token-env-var",
        "MCP_TOKEN",
    ])
    .expect("HTTP transport should parse");
    let Some(Subcommand::Mcp(mcp)) = cli.subcommand else {
        panic!("expected mcp subcommand");
    };
    let crate::mcp_cmd::McpSubcommand::Add(add) = mcp.subcommand else {
        panic!("expected mcp add");
    };
    assert!(add.transport_args.command.is_empty());
    assert_eq!(
        add.transport_args.url.as_deref(),
        Some("https://example.test/mcp")
    );
    assert_eq!(
        add.transport_args.bearer_token_env_var.as_deref(),
        Some("MCP_TOKEN")
    );

    for args in [
        &["chaos", "mcp", "add", "missing"][..],
        &[
            "chaos",
            "mcp",
            "add",
            "both",
            "--url",
            "https://example.test/mcp",
            "--",
            "node",
        ][..],
        &[
            "chaos",
            "mcp",
            "add",
            "remote",
            "--url",
            "https://example.test/mcp",
            "--env",
            "A=B",
        ][..],
        &[
            "chaos",
            "mcp",
            "add",
            "local",
            "--bearer-token",
            "secret",
            "--",
            "node",
        ][..],
    ] {
        assert!(
            try_parse_cli(args).is_err(),
            "invalid MCP transport should fail: {args:?}"
        );
    }
}

fn completion_shells_and_global_provider_are_preserved() {
    let cli = try_parse_cli(&["chaos", "completion"]).expect("default shell should parse");
    let Some(Subcommand::Completion(completion)) = cli.subcommand else {
        panic!("expected completion command");
    };
    assert_eq!(completion.shell, CompletionShell::Bash);

    for (name, expected) in [
        ("nushell", CompletionShell::Nushell),
        ("nu", CompletionShell::Nushell),
        ("powershell", CompletionShell::PowerShell),
        ("pwsh", CompletionShell::PowerShell),
    ] {
        let cli = try_parse_cli(&["chaos", "completion", name]).expect("shell alias should parse");
        let Some(Subcommand::Completion(completion)) = cli.subcommand else {
            panic!("expected completion command");
        };
        assert_eq!(completion.shell, expected);
    }

    let script = MultitoolCli::completion_script(usage::complete::Shell::Bash);
    assert!(!script.is_empty());
    assert!(script.contains("chaos"));

    for args in [
        &["chaos", "--provider", "anthropic", "models"][..],
        &["chaos", "models", "--provider", "anthropic", "--refresh"][..],
    ] {
        let cli = try_parse_cli(args).expect("global provider should parse in either position");
        assert_eq!(cli.provider.as_deref(), Some("anthropic"));
        let Some(Subcommand::Models(models)) = cli.subcommand else {
            panic!("expected models command");
        };
        assert_eq!(models.refresh, args.contains(&"--refresh"));
    }
}

fn global_config_order_and_duplicate_scalar_rejection_are_preserved() {
    let cli = try_parse_cli(&[
        "chaos",
        "-c",
        "model=first",
        "mcp",
        "-c",
        "model=second",
        "list",
    ])
    .expect("global config flags should parse around subcommands");
    assert_eq!(
        cli.config_overrides.raw_overrides,
        ["model=first", "model=second"]
    );

    assert!(try_parse_cli(&["chaos", "--provider", "openai", "--provider", "anthropic",]).is_err());
}
