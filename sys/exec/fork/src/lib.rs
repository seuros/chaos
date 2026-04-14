// - In the default output mode, it is paramount that the only thing written to
//   stdout is the final message (if any).
// - In --json mode, stdout must be valid JSONL, one event per line.
// For both modes, any other output must be written to stderr.
#![deny(clippy::print_stdout)]

mod cli;
mod event_processor;
mod event_processor_with_human_output;
pub mod event_processor_with_jsonl_output;
pub mod exec_events;

use chaos_argv::Arg0DispatchPaths;
use chaos_ipc::ProcessId;
use chaos_ipc::config_types::SandboxMode;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::ReviewRequest;
use chaos_ipc::protocol::ReviewTarget;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::user_input::UserInput;
use chaos_kern::AuthManager;
use chaos_kern::Process;
use chaos_kern::ProcessTable;
use chaos_kern::auth::enforce_login_restrictions;
use chaos_kern::check_execpolicy_for_warnings;
use chaos_kern::config::Config;
use chaos_kern::config::ConfigBuilder;
use chaos_kern::config::ConfigOverrides;
use chaos_kern::config::load_config_as_toml_with_cli_overrides;
use chaos_kern::config::resolve_oss_provider;
use chaos_kern::config_loader::ConfigLoadError;
use chaos_kern::config_loader::format_config_error_with_source;
use chaos_kern::format_exec_policy_error_with_source;
use chaos_kern::git_info::get_git_repo_root;
use chaos_kern::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use chaos_pwd::find_chaos_home;
use chaos_realpath::AbsolutePathBuf;
use chaos_syslog::set_parent_from_context;
use chaos_syslog::traceparent_context_from_env;
pub use cli::Cli;
pub use cli::Command;
pub use cli::ReviewArgs;
use event_processor_with_human_output::EventProcessorWithHumanOutput;
use event_processor_with_jsonl_output::EventProcessorWithJsonOutput;
use serde_json::Value;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::IsTerminal;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use supports_color::Stream;
use tokio::sync::mpsc;
use tracing::Instrument;
use tracing::error;
use tracing::field;
use tracing::info;
use tracing::info_span;
use tracing::warn;
use tracing_appender::non_blocking;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;
use uuid::Uuid;

use crate::cli::Command as ExecCommand;
use crate::event_processor::ChaosStatus;
use crate::event_processor::EventProcessor;
use chaos_kern::default_client::set_default_originator;

const DEFAULT_ANALYTICS_ENABLED: bool = true;
const DEBUG_LOG_PATH_ENV_VAR: &str = "CHAOS_DEBUG_LOG_PATH";
const DEBUG_LOG_FILTER: &str = "warn,chaos_kern=debug,chaos_boot=debug,chaos_fork=debug,\
chaos_console=debug,chaos_mcpd=debug,chaos_pam=debug,chaos_syslog=debug,\
chaos_ipc=debug,chaos_selinux=debug,chaos_dtrace=debug,chaos_hallucinate=debug,\
mcp_guest=debug,chaos_clamp=debug";

fn init_optional_debug_file_layer() -> anyhow::Result<(
    Option<impl tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync + 'static>,
    Option<WorkerGuard>,
)> {
    let Some(path) = std::env::var_os(DEBUG_LOG_PATH_ENV_VAR).map(PathBuf::from) else {
        return Ok((None, None));
    };

    let mut log_file_opts = OpenOptions::new();
    log_file_opts.create(true).append(true);

    {
        use std::os::unix::fs::OpenOptionsExt;
        log_file_opts.mode(0o600);
    }

    let log_file = log_file_opts.open(&path)?;
    let (non_blocking, guard) = non_blocking(log_file);
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEBUG_LOG_FILTER));
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(non_blocking)
        .with_target(true)
        .with_filter(filter);
    Ok((Some(layer), Some(guard)))
}

enum InitialOperation {
    UserTurn {
        items: Vec<UserInput>,
        output_schema: Option<Value>,
    },
    Review {
        review_request: ReviewRequest,
    },
}

struct ExecRunArgs {
    process_table: Arc<ProcessTable>,
    auth_manager: Arc<AuthManager>,
    command: Option<ExecCommand>,
    config: Config,
    cursor_ansi: bool,
    dangerously_bypass_approvals_and_sandbox: bool,
    exec_span: tracing::Span,
    images: Vec<PathBuf>,
    json_mode: bool,
    last_message_file: Option<PathBuf>,
    model_provider: Option<String>,
    oss: bool,
    output_schema_path: Option<PathBuf>,
    prompt: Option<String>,
    skip_git_repo_check: bool,
    stderr_with_ansi: bool,
}

fn exec_root_span() -> tracing::Span {
    info_span!(
        "chaos.exec",
        otel.kind = "internal",
        thread.id = field::Empty,
        turn.id = field::Empty,
    )
}

pub async fn run_main(cli: Cli, arg0_paths: Arg0DispatchPaths) -> anyhow::Result<()> {
    if let Err(err) = set_default_originator("chaos_fork".to_string()) {
        tracing::warn!(?err, "Failed to set chaos exec originator override {err:?}");
    }

    let Cli {
        command,
        images,
        model: model_cli_arg,
        oss,
        oss_provider,
        config_profile,
        full_auto,
        dangerously_bypass_approvals_and_sandbox,
        cwd,
        skip_git_repo_check,
        add_dir,
        ephemeral,
        color,
        last_message_file,
        json: json_mode,
        sandbox_mode: sandbox_mode_cli_arg,
        prompt,
        output_schema: output_schema_path,
        config_overrides,
        progress_cursor,
    } = cli;

    let (_stdout_with_ansi, stderr_with_ansi) = match color {
        cli::Color::Always => (true, true),
        cli::Color::Never => (false, false),
        cli::Color::Auto => (
            supports_color::on_cached(Stream::Stdout).is_some(),
            supports_color::on_cached(Stream::Stderr).is_some(),
        ),
    };
    let cursor_ansi = if progress_cursor {
        true
    } else {
        match color {
            cli::Color::Never => false,
            cli::Color::Always => true,
            cli::Color::Auto => {
                if stderr_with_ansi || std::io::stderr().is_terminal() {
                    true
                } else {
                    match std::env::var("TERM") {
                        Ok(term) => !term.is_empty() && term != "dumb",
                        Err(_) => false,
                    }
                }
            }
        }
    };

    // Build fmt layer (existing logging) to compose with OTEL layer.
    let default_level = "error";

    // Build env_filter separately and attach via with_filter.
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_level))
        .unwrap_or_else(|_| EnvFilter::new(default_level));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(stderr_with_ansi)
        .with_writer(std::io::stderr)
        .with_filter(env_filter);

    let sandbox_mode = if full_auto {
        Some(SandboxMode::WorkspaceWrite)
    } else if dangerously_bypass_approvals_and_sandbox {
        Some(SandboxMode::RootAccess)
    } else {
        sandbox_mode_cli_arg.map(Into::<SandboxMode>::into)
    };

    // Parse `-c` overrides from the CLI.
    let cli_kv_overrides = match config_overrides.parse_overrides() {
        Ok(v) => v,
        #[allow(clippy::print_stderr)]
        Err(e) => {
            eprintln!("Error parsing -c overrides: {e}");
            std::process::exit(1);
        }
    };

    let resolved_cwd = cwd.clone();
    let config_cwd = match resolved_cwd.as_deref() {
        Some(path) => AbsolutePathBuf::from_absolute_path(path.canonicalize()?)?,
        None => AbsolutePathBuf::current_dir()?,
    };

    // we load config.toml here to determine project state.
    #[allow(clippy::print_stderr)]
    let chaos_home = match find_chaos_home() {
        Ok(chaos_home) => chaos_home,
        Err(err) => {
            eprintln!("Error finding chaos home: {err}");
            std::process::exit(1);
        }
    };

    #[allow(clippy::print_stderr)]
    let config_toml = match load_config_as_toml_with_cli_overrides(
        &chaos_home,
        &config_cwd,
        cli_kv_overrides.clone(),
    )
    .await
    {
        Ok(config_toml) => config_toml,
        Err(err) => {
            let config_error = err
                .get_ref()
                .and_then(|err| err.downcast_ref::<ConfigLoadError>())
                .map(ConfigLoadError::config_error);
            if let Some(config_error) = config_error {
                eprintln!(
                    "Error loading config.toml:\n{}",
                    format_config_error_with_source(config_error)
                );
            } else {
                eprintln!("Error loading config.toml: {err}");
            }
            std::process::exit(1);
        }
    };

    let model_provider = if oss {
        let resolved = resolve_oss_provider(
            oss_provider.as_deref(),
            &config_toml,
            config_profile.clone(),
        );

        if let Some(provider) = resolved {
            Some(provider)
        } else {
            return Err(anyhow::anyhow!(
                "No default OSS provider configured. Use --local-provider=provider or set oss_provider in config.toml"
            ));
        }
    } else {
        None // No OSS mode enabled
    };

    // When using `--oss`, let the bootstrapper pick the model based on selected provider
    let model = if let Some(model) = model_cli_arg {
        Some(model)
    } else if oss {
        // No built-in default model for generic OSS providers; callers specify via --model.
        None
    } else {
        None // No model specified, will use the default.
    };

    // Load configuration and determine approval policy
    let overrides = ConfigOverrides {
        model,
        review_model: None,
        config_profile,
        // Default to never ask for approvals in headless mode. Feature flags can override.
        approval_policy: Some(ApprovalPolicy::Headless),
        approvals_reviewer: None,
        sandbox_mode,
        cwd: resolved_cwd,
        model_provider: model_provider.clone(),
        service_tier: None,
        alcatraz_linux_exe: arg0_paths.alcatraz_linux_exe.clone(),
        alcatraz_freebsd_exe: arg0_paths.alcatraz_freebsd_exe.clone(),
        alcatraz_macos_exe: arg0_paths.alcatraz_macos_exe.clone(),
        base_instructions: None,
        minion_instructions: None,
        personality: None,
        compact_prompt: None,
        show_raw_agent_reasoning: oss.then_some(true),
        ephemeral: ephemeral.then_some(true),
        additional_writable_roots: add_dir,
    };

    let config = ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .harness_overrides(overrides)
        .build()
        .await?;

    #[allow(clippy::print_stderr)]
    match check_execpolicy_for_warnings(&config.config_layer_stack).await {
        Ok(None) => {}
        Ok(Some(err)) | Err(err) => {
            eprintln!(
                "Error loading rules:\n{}",
                format_exec_policy_error_with_source(&err)
            );
            std::process::exit(1);
        }
    }

    if let Err(err) = enforce_login_restrictions(&config) {
        eprintln!("{err}");
        std::process::exit(1);
    }

    let otel = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        chaos_kern::otel_init::build_provider(
            &config,
            CHAOS_VERSION,
            /*service_name_override*/ None,
            DEFAULT_ANALYTICS_ENABLED,
        )
    })) {
        Ok(Ok(otel)) => otel,
        Ok(Err(e)) => {
            eprintln!("Could not create otel exporter: {e}");
            None
        }
        Err(_) => {
            eprintln!("Could not create otel exporter: panicked during initialization");
            None
        }
    };

    let otel_logger_layer = otel.as_ref().and_then(|o| o.logger_layer());

    let otel_tracing_layer = otel.as_ref().and_then(|o| o.tracing_layer());
    let (debug_file_layer, _debug_log_guard) = init_optional_debug_file_layer()?;

    let _ = tracing_subscriber::registry()
        .with(debug_file_layer)
        .with(fmt_layer)
        .with(otel_tracing_layer)
        .with(otel_logger_layer)
        .try_init();

    let exec_span = exec_root_span();
    if let Some(context) = traceparent_context_from_env() {
        set_parent_from_context(&exec_span, context);
    }

    // Create core managers directly (same pattern as the TUI).
    let auth_manager = AuthManager::shared(
        config.chaos_home.clone(),
        true, // enable_codex_api_key_env
        config.cli_auth_credentials_store_mode,
    );
    let process_table = Arc::new(ProcessTable::new(
        &config,
        auth_manager.clone(),
        SessionSource::Exec,
        CollaborationModesConfig {
            default_mode_request_user_input: true,
        },
    ));

    run_exec_session(ExecRunArgs {
        process_table,
        auth_manager,
        command,
        config,
        cursor_ansi,
        dangerously_bypass_approvals_and_sandbox,
        exec_span: exec_span.clone(),
        images,
        json_mode,
        last_message_file,
        model_provider,
        oss,
        output_schema_path,
        prompt,
        skip_git_repo_check,
        stderr_with_ansi,
    })
    .instrument(exec_span)
    .await
}

async fn run_exec_session(args: ExecRunArgs) -> anyhow::Result<()> {
    let ExecRunArgs {
        process_table,
        auth_manager,
        command,
        config,
        cursor_ansi,
        dangerously_bypass_approvals_and_sandbox,
        exec_span,
        images,
        json_mode,
        last_message_file,
        model_provider,
        oss,
        output_schema_path,
        prompt,
        skip_git_repo_check,
        stderr_with_ansi,
    } = args;

    let mut event_processor: Box<dyn EventProcessor> = match json_mode {
        true => Box::new(EventProcessorWithJsonOutput::new(last_message_file.clone())),
        _ => Box::new(EventProcessorWithHumanOutput::create_with_ansi(
            stderr_with_ansi,
            cursor_ansi,
            &config,
            last_message_file.clone(),
        )),
    };
    let required_mcp_servers: HashSet<String> = config
        .mcp_servers
        .get()
        .iter()
        .filter(|(_, server)| server.enabled && server.required)
        .map(|(name, _)| name.clone())
        .collect();

    if oss {
        // We're in the oss section, so provider_id should be Some
        // Let's handle None case gracefully though just in case
        let provider_id = match model_provider.as_ref() {
            Some(id) => id,
            None => {
                error!("OSS provider unexpectedly not set when oss flag is used");
                return Err(anyhow::anyhow!(
                    "OSS provider not set but oss flag was used"
                ));
            }
        };
        // OSS provider readiness checks (lmstudio/ollama) have been removed.
        // Provider connectivity is validated lazily on the first request.
        let _ = provider_id;
    }

    let default_cwd = config.cwd.to_path_buf();
    let default_approval_policy = config.permissions.approval_policy.value();
    let default_sandbox_policy = config.permissions.sandbox_policy.get();
    let default_effort = config.model_reasoning_effort;

    // When --yolo (dangerously_bypass_approvals_and_sandbox) is set, also skip the git repo check
    // since the user is explicitly running in an externally sandboxed environment.
    if !skip_git_repo_check
        && !dangerously_bypass_approvals_and_sandbox
        && get_git_repo_root(&default_cwd).is_none()
    {
        eprintln!("Not inside a trusted directory and --skip-git-repo-check was not specified.");
        std::process::exit(1);
    }

    // Start or resume a process directly via the ProcessTable.
    let new_process = if let Some(ExecCommand::Resume(ref resume_args)) = command {
        let resume_process_id = resolve_resume_process_id(&config, resume_args).await?;

        if let Some(process_id) = resume_process_id {
            process_table
                .resume_process(
                    config.clone(),
                    process_id,
                    auth_manager.clone(),
                    /*parent_trace*/ None,
                )
                .await
                .map_err(|err| anyhow::anyhow!("failed to resume process: {err}"))?
        } else {
            process_table
                .start_process(config.clone())
                .await
                .map_err(|err| anyhow::anyhow!("failed to start process: {err}"))?
        }
    } else {
        process_table
            .start_process(config.clone())
            .await
            .map_err(|err| anyhow::anyhow!("failed to start process: {err}"))?
    };
    let (_, thread, session_configured) = new_process.into_parts();

    let primary_process_id_for_span = session_configured.session_id.to_string();
    exec_span.record("thread.id", primary_process_id_for_span.as_str());

    let (initial_operation, prompt_summary) = match (command.as_ref(), prompt, images) {
        (Some(ExecCommand::Review(review_cli)), _, _) => {
            let review_request = build_review_request(review_cli)?;
            let summary = chaos_kern::review_prompts::user_facing_hint(&review_request.target);
            (InitialOperation::Review { review_request }, summary)
        }
        (Some(ExecCommand::Resume(args)), root_prompt, imgs) => {
            let prompt_arg = args
                .prompt
                .clone()
                .or_else(|| {
                    if args.last {
                        args.session_id.clone()
                    } else {
                        None
                    }
                })
                .or(root_prompt);
            let prompt_text = resolve_prompt(prompt_arg);
            let mut items: Vec<UserInput> = imgs
                .into_iter()
                .chain(args.images.iter().cloned())
                .map(|path| UserInput::LocalImage { path })
                .collect();
            items.push(UserInput::Text {
                text: prompt_text.clone(),
                // CLI input doesn't track UI element ranges, so none are available here.
                text_elements: Vec::new(),
            });
            let output_schema = load_output_schema(output_schema_path.clone());
            (
                InitialOperation::UserTurn {
                    items,
                    output_schema,
                },
                prompt_text,
            )
        }
        (None, root_prompt, imgs) => {
            let prompt_text = resolve_prompt(root_prompt);
            let mut items: Vec<UserInput> = imgs
                .into_iter()
                .map(|path| UserInput::LocalImage { path })
                .collect();
            items.push(UserInput::Text {
                text: prompt_text.clone(),
                // CLI input doesn't track UI element ranges, so none are available here.
                text_elements: Vec::new(),
            });
            let output_schema = load_output_schema(output_schema_path);
            (
                InitialOperation::UserTurn {
                    items,
                    output_schema,
                },
                prompt_text,
            )
        }
    };

    // Print the effective configuration and initial request so users can see what Chaos
    // is using.
    event_processor.print_config_summary(&config, &prompt_summary, &session_configured);

    info!("Chaos initialized with event: {session_configured:?}");

    let (interrupt_tx, mut interrupt_rx) = mpsc::unbounded_channel::<()>();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::debug!("Keyboard interrupt");
            let _ = interrupt_tx.send(());
        }
    });

    // Submit the initial operation to the process.
    let task_id = match initial_operation {
        InitialOperation::UserTurn {
            items,
            output_schema,
        } => {
            let task_id = thread
                .submit(Op::UserTurn {
                    items: items.into_iter().collect(),
                    cwd: default_cwd,
                    approval_policy: default_approval_policy,
                    sandbox_policy: default_sandbox_policy.clone(),
                    model: session_configured.model.clone(),
                    effort: default_effort,
                    summary: None,
                    service_tier: None,
                    final_output_json_schema: output_schema,
                    collaboration_mode: None,
                    personality: None,
                })
                .await
                .map_err(|err| anyhow::anyhow!("failed to submit user turn: {err}"))?;
            info!("Sent prompt with event ID: {task_id}");
            task_id
        }
        InitialOperation::Review { review_request } => {
            let task_id = thread
                .submit(Op::Review { review_request })
                .await
                .map_err(|err| anyhow::anyhow!("failed to submit review: {err}"))?;
            info!("Sent review request with event ID: {task_id}");
            task_id
        }
    };
    exec_span.record("turn.id", task_id.as_str());

    // Run the event loop until the task is complete.
    // Track whether a fatal error was reported so we can exit with a non-zero
    // status for automation-friendly signaling.
    let mut error_seen = false;
    let mut interrupt_channel_open = true;
    run_event_loop(
        &thread,
        &mut *event_processor,
        &task_id,
        &required_mcp_servers,
        &mut error_seen,
        &mut interrupt_rx,
        &mut interrupt_channel_open,
    )
    .await;

    if let Err(err) = thread.shutdown_and_wait().await {
        warn!("process shutdown failed: {err}");
    }
    event_processor.print_final_output();
    if error_seen {
        std::process::exit(1);
    }

    Ok(())
}

/// Core event loop: reads events from the process and dispatches them
/// to the event processor, handling interrupts and shutdown.
async fn run_event_loop(
    thread: &Arc<Process>,
    event_processor: &mut dyn EventProcessor,
    task_id: &str,
    required_mcp_servers: &HashSet<String>,
    error_seen: &mut bool,
    interrupt_rx: &mut mpsc::UnboundedReceiver<()>,
    interrupt_channel_open: &mut bool,
) {
    loop {
        let event = tokio::select! {
            maybe_interrupt = interrupt_rx.recv(), if *interrupt_channel_open => {
                if maybe_interrupt.is_none() {
                    *interrupt_channel_open = false;
                    continue;
                }
                if let Err(err) = thread.submit(Op::Interrupt).await {
                    warn!("interrupt submit failed: {err}");
                }
                continue;
            }
            result = thread.next_event() => {
                match result {
                    Ok(event) => event,
                    Err(err) => {
                        warn!("event stream ended: {err}");
                        break;
                    }
                }
            }
        };

        // Check for fatal errors.
        if matches!(&event.msg, EventMsg::Error(_)) {
            *error_seen = true;
        }

        // Filter events not relevant to our turn.
        match &event.msg {
            EventMsg::TurnComplete(payload) => {
                if payload.turn_id != task_id {
                    continue;
                }
            }
            EventMsg::TurnAborted(payload) => {
                if payload.turn_id.as_deref() != Some(task_id) {
                    continue;
                }
            }
            EventMsg::McpStartupUpdate(update) => {
                if required_mcp_servers.contains(&update.server)
                    && let chaos_ipc::protocol::McpStartupStatus::Failed { error } = &update.status
                {
                    *error_seen = true;
                    eprintln!(
                        "Required MCP server '{}' failed to initialize: {error}",
                        update.server
                    );
                    if let Err(err) = thread.submit(Op::Interrupt).await {
                        warn!("interrupt submit failed during MCP shutdown: {err}");
                    }
                    break;
                }
            }
            // Skip redundant SessionConfigured events from the stream --
            // we already have the authoritative one from start/resume.
            EventMsg::SessionConfigured(_) => {
                continue;
            }
            _ => {}
        }

        match event_processor.process_event(event) {
            ChaosStatus::Running => {}
            ChaosStatus::InitiateShutdown | ChaosStatus::Shutdown => {
                break;
            }
        }
    }
}

async fn resolve_resume_process_id(
    config: &Config,
    args: &crate::cli::ResumeArgs,
) -> anyhow::Result<Option<ProcessId>> {
    if args.last {
        let filter_cwd = if args.all {
            None
        } else {
            Some(config.cwd.as_path())
        };
        match chaos_kern::RolloutRecorder::find_latest_process_id(
            config,
            /*page_size*/ 1,
            /*cursor*/ None,
            chaos_kern::ProcessSortKey::UpdatedAt,
            &[],
            filter_cwd,
        )
        .await
        {
            Ok(process_id) => Ok(process_id),
            Err(e) => {
                error!("Error listing processes: {e}");
                Ok(None)
            }
        }
    } else if let Some(id_str) = args.session_id.as_deref() {
        if Uuid::parse_str(id_str).is_ok() {
            let process_id = ProcessId::from_string(id_str)?;
            if chaos_kern::RolloutRecorder::journal_contains_process(process_id).await? {
                Ok(Some(process_id))
            } else {
                Ok(None)
            }
        } else {
            let process_id =
                chaos_kern::find_process_id_by_name(&config.chaos_home, id_str).await?;
            if let Some(process_id) = process_id
                && chaos_kern::RolloutRecorder::journal_contains_process(process_id).await?
            {
                Ok(Some(process_id))
            } else {
                Ok(None)
            }
        }
    } else {
        Ok(None)
    }
}

fn load_output_schema(path: Option<PathBuf>) -> Option<Value> {
    let path = path?;

    let schema_str = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) => {
            eprintln!(
                "Failed to read output schema file {}: {err}",
                path.display()
            );
            std::process::exit(1);
        }
    };

    match serde_json::from_str::<Value>(&schema_str) {
        Ok(value) => Some(value),
        Err(err) => {
            eprintln!(
                "Output schema file {} is not valid JSON: {err}",
                path.display()
            );
            std::process::exit(1);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PromptDecodeError {
    InvalidUtf8 { valid_up_to: usize },
    InvalidUtf16 { encoding: &'static str },
    UnsupportedBom { encoding: &'static str },
}

impl std::fmt::Display for PromptDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PromptDecodeError::InvalidUtf8 { valid_up_to } => write!(
                f,
                "input is not valid UTF-8 (invalid byte at offset {valid_up_to}). Convert it to UTF-8 and retry (e.g., `iconv -f <ENC> -t UTF-8 prompt.txt`)."
            ),
            PromptDecodeError::InvalidUtf16 { encoding } => write!(
                f,
                "input looked like {encoding} but could not be decoded. Convert it to UTF-8 and retry."
            ),
            PromptDecodeError::UnsupportedBom { encoding } => write!(
                f,
                "input appears to be {encoding}. Convert it to UTF-8 and retry."
            ),
        }
    }
}

fn decode_prompt_bytes(input: &[u8]) -> Result<String, PromptDecodeError> {
    let input = input.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(input);

    if input.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        return Err(PromptDecodeError::UnsupportedBom {
            encoding: "UTF-32LE",
        });
    }

    if input.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        return Err(PromptDecodeError::UnsupportedBom {
            encoding: "UTF-32BE",
        });
    }

    if let Some(rest) = input.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(rest, "UTF-16LE", u16::from_le_bytes);
    }

    if let Some(rest) = input.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(rest, "UTF-16BE", u16::from_be_bytes);
    }

    std::str::from_utf8(input)
        .map(str::to_string)
        .map_err(|e| PromptDecodeError::InvalidUtf8 {
            valid_up_to: e.valid_up_to(),
        })
}

fn decode_utf16(
    input: &[u8],
    encoding: &'static str,
    decode_unit: fn([u8; 2]) -> u16,
) -> Result<String, PromptDecodeError> {
    if !input.len().is_multiple_of(2) {
        return Err(PromptDecodeError::InvalidUtf16 { encoding });
    }

    let units: Vec<u16> = input
        .chunks_exact(2)
        .map(|chunk| decode_unit([chunk[0], chunk[1]]))
        .collect();

    String::from_utf16(&units).map_err(|_| PromptDecodeError::InvalidUtf16 { encoding })
}

fn resolve_prompt(prompt_arg: Option<String>) -> String {
    match prompt_arg {
        Some(p) if p != "-" => p,
        maybe_dash => {
            let force_stdin = matches!(maybe_dash.as_deref(), Some("-"));

            if std::io::stdin().is_terminal() && !force_stdin {
                eprintln!(
                    "No prompt provided. Either specify one as an argument or pipe the prompt into stdin."
                );
                std::process::exit(1);
            }

            if !force_stdin {
                eprintln!("Reading prompt from stdin...");
            }

            let mut bytes = Vec::new();
            if let Err(e) = std::io::stdin().read_to_end(&mut bytes) {
                eprintln!("Failed to read prompt from stdin: {e}");
                std::process::exit(1);
            }

            let buffer = match decode_prompt_bytes(&bytes) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to read prompt from stdin: {e}");
                    std::process::exit(1);
                }
            };

            if buffer.trim().is_empty() {
                eprintln!("No prompt provided via stdin.");
                std::process::exit(1);
            }
            buffer
        }
    }
}

fn build_review_request(args: &ReviewArgs) -> anyhow::Result<ReviewRequest> {
    let target = if args.uncommitted {
        ReviewTarget::UncommittedChanges
    } else if let Some(branch) = args.base.clone() {
        ReviewTarget::BaseBranch { branch }
    } else if let Some(sha) = args.commit.clone() {
        ReviewTarget::Commit {
            sha,
            title: args.commit_title.clone(),
        }
    } else if let Some(prompt_arg) = args.prompt.clone() {
        let prompt = resolve_prompt(Some(prompt_arg)).trim().to_string();
        if prompt.is_empty() {
            anyhow::bail!("Review prompt cannot be empty");
        }
        ReviewTarget::Custom {
            instructions: prompt,
        }
    } else {
        anyhow::bail!(
            "Specify --uncommitted, --base, --commit, or provide custom review instructions"
        );
    };

    Ok(ReviewRequest {
        target,
        user_facing_hint: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_syslog::set_parent_from_w3c_trace_context;
    use pretty_assertions::assert_eq;
    use rama::telemetry::opentelemetry::sdk::trace::SdkTracerProvider;
    use rama::telemetry::opentelemetry::trace::TraceContextExt;
    use rama::telemetry::opentelemetry::trace::TraceId;
    use rama::telemetry::opentelemetry::trace::TracerProvider as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    fn test_tracing_subscriber() -> impl tracing::Subscriber + Send + Sync {
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("chaos-exec-tests");
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer))
    }

    #[test]
    fn exec_defaults_analytics_to_enabled() {
        assert_eq!(DEFAULT_ANALYTICS_ENABLED, true);
    }

    #[test]
    fn exec_root_span_can_be_parented_from_trace_context() {
        let subscriber = test_tracing_subscriber();
        let _guard = tracing::subscriber::set_default(subscriber);

        let parent = chaos_ipc::protocol::W3cTraceContext {
            traceparent: Some("00-00000000000000000000000000000077-0000000000000088-01".into()),
            tracestate: Some("vendor=value".into()),
        };
        let exec_span = exec_root_span();
        assert!(set_parent_from_w3c_trace_context(&exec_span, &parent));

        let trace_id = exec_span.context().span().span_context().trace_id();
        assert_eq!(
            trace_id,
            TraceId::from_hex("00000000000000000000000000000077").expect("trace id")
        );
    }

    #[test]
    fn builds_uncommitted_review_request() {
        let args = ReviewArgs {
            uncommitted: true,
            base: None,
            commit: None,
            commit_title: None,
            prompt: None,
        };
        let request = build_review_request(&args).expect("builds uncommitted review request");

        let expected = ReviewRequest {
            target: ReviewTarget::UncommittedChanges,
            user_facing_hint: None,
        };

        assert_eq!(request, expected);
    }

    #[test]
    fn builds_commit_review_request_with_title() {
        let args = ReviewArgs {
            uncommitted: false,
            base: None,
            commit: Some("123456789".to_string()),
            commit_title: Some("Add review command".to_string()),
            prompt: None,
        };
        let request = build_review_request(&args).expect("builds commit review request");

        let expected = ReviewRequest {
            target: ReviewTarget::Commit {
                sha: "123456789".to_string(),
                title: Some("Add review command".to_string()),
            },
            user_facing_hint: None,
        };

        assert_eq!(request, expected);
    }

    #[test]
    fn builds_custom_review_request_trims_prompt() {
        let args = ReviewArgs {
            uncommitted: false,
            base: None,
            commit: None,
            commit_title: None,
            prompt: Some("  custom review instructions  ".to_string()),
        };
        let request = build_review_request(&args).expect("builds custom review request");

        let expected = ReviewRequest {
            target: ReviewTarget::Custom {
                instructions: "custom review instructions".to_string(),
            },
            user_facing_hint: None,
        };

        assert_eq!(request, expected);
    }

    #[test]
    fn decode_prompt_bytes_strips_utf8_bom() {
        let input = [0xEF, 0xBB, 0xBF, b'h', b'i', b'\n'];

        let out = decode_prompt_bytes(&input).expect("decode utf-8 with BOM");

        assert_eq!(out, "hi\n");
    }

    #[test]
    fn decode_prompt_bytes_decodes_utf16le_bom() {
        // UTF-16LE BOM + "hi\n"
        let input = [0xFF, 0xFE, b'h', 0x00, b'i', 0x00, b'\n', 0x00];

        let out = decode_prompt_bytes(&input).expect("decode utf-16le with BOM");

        assert_eq!(out, "hi\n");
    }

    #[test]
    fn decode_prompt_bytes_decodes_utf16be_bom() {
        // UTF-16BE BOM + "hi\n"
        let input = [0xFE, 0xFF, 0x00, b'h', 0x00, b'i', 0x00, b'\n'];

        let out = decode_prompt_bytes(&input).expect("decode utf-16be with BOM");

        assert_eq!(out, "hi\n");
    }

    #[test]
    fn decode_prompt_bytes_rejects_utf32le_bom() {
        // UTF-32LE BOM + "hi\n"
        let input = [
            0xFF, 0xFE, 0x00, 0x00, b'h', 0x00, 0x00, 0x00, b'i', 0x00, 0x00, 0x00, b'\n', 0x00,
            0x00, 0x00,
        ];

        let err = decode_prompt_bytes(&input).expect_err("utf-32le should be rejected");

        assert_eq!(
            err,
            PromptDecodeError::UnsupportedBom {
                encoding: "UTF-32LE"
            }
        );
    }

    #[test]
    fn decode_prompt_bytes_rejects_utf32be_bom() {
        // UTF-32BE BOM + "hi\n"
        let input = [
            0x00, 0x00, 0xFE, 0xFF, 0x00, 0x00, 0x00, b'h', 0x00, 0x00, 0x00, b'i', 0x00, 0x00,
            0x00, b'\n',
        ];

        let err = decode_prompt_bytes(&input).expect_err("utf-32be should be rejected");

        assert_eq!(
            err,
            PromptDecodeError::UnsupportedBom {
                encoding: "UTF-32BE"
            }
        );
    }

    #[test]
    fn decode_prompt_bytes_rejects_invalid_utf8() {
        // Invalid UTF-8 sequence: 0xC3 0x28
        let input = [0xC3, 0x28];

        let err = decode_prompt_bytes(&input).expect_err("invalid utf-8 should fail");

        assert_eq!(err, PromptDecodeError::InvalidUtf8 { valid_up_to: 0 });
    }
}
