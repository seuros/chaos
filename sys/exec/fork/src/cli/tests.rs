use super::*;
use chaos_test_fixtures::TEST_MODEL;
use pretty_assertions::assert_eq;

fn try_parse<'v>(args: &[&'v str]) -> Result<Cli, usage::Error<'static, 'v>> {
    try_parse_for_test(args)
}

fn parse(args: &[&str]) -> Cli {
    try_parse(args).expect("valid exec arguments")
}

#[test]
fn resume_rejects_model_flag_after_subcommand() {
    const PROMPT: &str = "echo resume-with-global-flags-after-subcommand";
    let err = try_parse(&[
        "chaos-exec",
        "resume",
        "--last",
        "--json",
        "--model",
        TEST_MODEL,
        "--headless",
        "--skip-git-repo-check",
        "--ephemeral",
        PROMPT,
    ])
    .expect_err("--model is a root exec override, not a resume flag");

    assert!(matches!(
        err,
        usage::Error::UnknownFlag { .. } | usage::Error::UnexpectedArg { .. }
    ));
}

#[test]
fn resume_parses_prompt_after_global_flags() {
    const PROMPT: &str = "echo resume-with-global-flags-after-subcommand";
    let cli = parse(&[
        "chaos-exec",
        "resume",
        "--last",
        "--json",
        "--headless",
        "--skip-git-repo-check",
        "--ephemeral",
        PROMPT,
    ]);

    assert!(cli.ephemeral);
    let Some(Command::Resume(args)) = cli.command else {
        panic!("expected resume command");
    };
    let effective_prompt = args.prompt.clone().or_else(|| {
        if args.last {
            args.session_id.clone()
        } else {
            None
        }
    });
    assert_eq!(effective_prompt.as_deref(), Some(PROMPT));
}

#[test]
fn resume_accepts_output_last_message_flag_after_subcommand() {
    const PROMPT: &str = "echo resume-with-output-file";
    let cli = parse(&[
        "chaos-exec",
        "resume",
        "session-123",
        "-o",
        "/tmp/resume-output.md",
        PROMPT,
    ]);

    assert_eq!(
        cli.last_message_file,
        Some(PathBuf::from("/tmp/resume-output.md"))
    );
    let Some(Command::Resume(args)) = cli.command else {
        panic!("expected resume command");
    };
    assert_eq!(args.session_id.as_deref(), Some("session-123"));
    assert_eq!(args.prompt.as_deref(), Some(PROMPT));
}

#[test]
fn hidden_fork_snapshot_option_is_parsed() {
    let cli = parse(&[
        "chaos-exec",
        "--fork-snapshot",
        "/tmp/turn-snapshot.json",
        "--ephemeral",
        "reflect",
    ]);

    assert_eq!(
        cli.fork_snapshot,
        Some(PathBuf::from("/tmp/turn-snapshot.json"))
    );
    assert!(cli.ephemeral);
    assert_eq!(cli.prompt.as_deref(), Some("reflect"));
}
