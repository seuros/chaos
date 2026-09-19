use super::*;
use assert_matches::assert_matches;
#[cfg(feature = "tui")]
use chaos_ipc::ProcessId;
#[cfg(feature = "tui")]
use chaos_ipc::protocol::TokenUsage;
use pretty_assertions::assert_eq;
use std::ffi::OsStr;

fn try_parse_cli<'v>(args: &[&'v str]) -> Result<MultitoolCli, usage::Error<'static, 'v>> {
    let argv = args.iter().map(|arg| OsStr::new(*arg)).collect::<Vec<_>>();
    let mut cli = MultitoolCli::try_parse_from(&argv)?;
    cli.normalize();
    Ok(cli)
}

#[test]
fn reflex_test_is_a_local_subcommand_with_global_config_flags() {
    let cli = try_parse_cli(&[
        "chaos",
        "-p",
        "work",
        "-c",
        "reflex.typesafe.timeout_ms=50",
        "reflex",
        "test",
    ])
    .unwrap();
    assert_eq!(cli.config_profile(), Some("work"));
    assert_eq!(
        cli.config_overrides.raw_overrides,
        vec!["reflex.typesafe.timeout_ms=50"]
    );
    assert_matches!(
        cli.subcommand,
        Some(Subcommand::Reflex(reflex_cmd::ReflexCommand {
            command: reflex_cmd::ReflexSubcommand::Test
        }))
    );
    assert!(try_parse_cli(&["chaos", "reflex", "unknown"]).is_err());
    assert!(try_parse_cli(&["chaos", "reflex", "test", "--api-key", "not-a-key"]).is_err());
}

#[cfg(feature = "tui")]
fn finalize_interactive_from_args(args: &[&str]) -> TuiCli {
    let MultitoolCli {
        interactive,
        config_overrides,
        subcommand,
        ..
    } = try_parse_cli(args).expect("parse");

    finalize_interactive(interactive, config_overrides, subcommand)
}

#[test]
fn cli_parser_and_exit_format_suite() {
    exec_resume_cli_parses_positionals_and_subcommand_flags();
    auto_exec_flags_do_not_leak_to_unrelated_subcommands();
    #[cfg(feature = "tui")]
    {
        format_exit_messages_handles_zero_usage_resume_color_and_thread_names();
        resume_and_fork_picker_logic_cover_default_last_session_and_all_modes();
        interactive_flags_and_prompts_keep_subcommand_precedence();
    }
    debug_flag_is_global_and_defaults_false();
    mcp_add_transport_shapes_and_constraints_are_preserved();
    completion_shells_and_global_provider_are_preserved();
    global_config_order_and_duplicate_scalar_rejection_are_preserved();
    headless_commands_and_profile_are_available();
    #[cfg(not(feature = "tui"))]
    headless_build_rejects_interactive_commands_and_flags();
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

#[cfg(feature = "tui")]
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

#[cfg(feature = "tui")]
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

#[cfg(feature = "tui")]
fn resume_and_fork_picker_logic_cover_default_last_session_and_all_modes() {
    for command in ["resume", "fork"] {
        for (flags, expected) in [
            (&[][..], (true, false, None, false)),
            (&["--last"][..], (false, true, None, false)),
            (&["1234"][..], (false, false, Some("1234"), false)),
            (&["--all"][..], (true, false, None, true)),
            (&["--last", "--all"][..], (false, true, None, true)),
            (&["1234", "--all"][..], (false, false, Some("1234"), true)),
        ] {
            let mut args = vec!["chaos", command];
            args.extend_from_slice(flags);
            let interactive = finalize_interactive_from_args(&args);
            let resume = (
                interactive.resume_picker,
                interactive.resume_last,
                interactive.resume_session_id.as_deref(),
                interactive.resume_show_all,
            );
            let fork = (
                interactive.fork_picker,
                interactive.fork_last,
                interactive.fork_session_id.as_deref(),
                interactive.fork_show_all,
            );
            let (active, inactive) = if command == "resume" {
                (resume, fork)
            } else {
                (fork, resume)
            };
            assert_eq!(active, expected, "{args:?}");
            assert_eq!(inactive, (false, false, None, false), "{args:?}");
        }
    }
    assert!(try_parse_cli(&["chaos", "fork", "1234", "--last"]).is_err());
    let interactive = finalize_interactive_from_args(&["chaos", "resume", "1234", "--last"]);
    assert!(interactive.resume_last);
    assert_eq!(interactive.resume_session_id.as_deref(), Some("1234"));
}

#[cfg(feature = "tui")]
fn interactive_flags_and_prompts_keep_subcommand_precedence() {
    let interactive = finalize_interactive_from_args(&[
        "chaos",
        "-p",
        "root",
        "-c",
        "model=root",
        "one\r\ntwo\rthree",
    ]);
    assert_eq!(interactive.config_profile.as_deref(), Some("root"));
    assert_eq!(interactive.config_overrides.raw_overrides, ["model=root"]);
    assert_eq!(interactive.prompt.as_deref(), Some("one\ntwo\nthree"));

    for command in ["resume", "fork"] {
        let interactive = finalize_interactive_from_args(&[
            "chaos",
            "-p",
            "root",
            "-c",
            "model=root",
            command,
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
            "-c",
            "model=child",
            "one\r\ntwo\rthree",
        ]);
        assert_eq!(interactive.config_profile.as_deref(), Some("my-profile"));
        assert_eq!(
            interactive.config_overrides.raw_overrides,
            ["model=root", "model=child"]
        );
        assert_eq!(interactive.prompt.as_deref(), Some("one\ntwo\nthree"));
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

        let interactive = finalize_interactive_from_args(&["chaos", command, "--headless"]);
        assert!(interactive.auto_exec.headless);
    }
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

fn headless_commands_and_profile_are_available() {
    let cli = try_parse_cli(&[
        "chaos",
        "--provider",
        "openai",
        "-c",
        "model=test",
        "serve",
        "--bearer-token",
        "test-token",
        "--port",
        "4040",
    ])
    .expect("HTTP server should parse");
    assert_eq!(cli.provider.as_deref(), Some("openai"));
    assert_eq!(cli.config_overrides.raw_overrides, ["model=test"]);
    let Some(Subcommand::Serve(serve)) = cli.subcommand else {
        panic!("expected serve");
    };
    assert_eq!(serve.port, 4040);
    assert_eq!(serve.bearer_token.as_deref(), Some("test-token"));

    let cli = try_parse_cli(&["chaos", "mcp", "serve"]).expect("MCP server should parse");
    assert_matches!(
        cli.subcommand,
        Some(Subcommand::Mcp(McpCli {
            subcommand: crate::mcp_cmd::McpSubcommand::Serve,
            ..
        }))
    );

    let cli = try_parse_cli(&["chaos", "taskd", "--once"]).expect("task supervisor should parse");
    assert_matches!(
        cli.subcommand,
        Some(Subcommand::Taskd(chaos_taskd::TaskdCli { once: true }))
    );
    let cli = try_parse_cli(&["chaos", "clamp-session-bridge"]).expect("bridge should parse");
    assert_matches!(cli.subcommand, Some(Subcommand::ClampSessionBridge));

    for flag in ["--profile", "-p"] {
        let cli = try_parse_cli(&["chaos", flag, "server", "models"])
            .expect("models profile should not depend on the TUI");
        assert_eq!(cli.config_profile(), Some("server"));
        assert_matches!(cli.subcommand, Some(Subcommand::Models(_)));
    }
}

#[cfg(not(feature = "tui"))]
fn headless_build_rejects_interactive_commands_and_flags() {
    for args in [
        &["chaos", "resume"][..],
        &["chaos", "fork"][..],
        &["chaos", "hello"][..],
        &["chaos", "--no-alt-screen"][..],
        &["chaos", "--clamp"][..],
    ] {
        assert!(try_parse_cli(args).is_err(), "TUI input accepted: {args:?}");
    }
}
