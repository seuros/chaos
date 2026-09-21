// Forbid accidental stdout/stderr writes in the *library* portion of the TUI.
// The standalone `chaos-console` binary prints a short help message before the
// alternate‑screen mode starts; that file opts‑out locally via `allow`.
#![deny(clippy::print_stdout, clippy::print_stderr)]
#![deny(clippy::disallowed_methods)]
use additional_dirs::add_dir_warning_message;
use app::App;
pub use app::AppExitInfo;
pub use app::ExitReason;
use chaos_coreboot::CoreBoot;
use chaos_getopt::auto_exec_approval_policy;
use chaos_ipc::ProcessId;
use chaos_ipc::config_types::AltScreenMode;
use chaos_ipc::config_types::SandboxMode;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_ipc::product::OS_NAME;
use chaos_kern::INTERACTIVE_SESSION_SOURCES;
use chaos_kern::ProcessSortKey;
use chaos_kern::RolloutRecorder;
use chaos_kern::auth::enforce_login_restrictions;
use chaos_kern::check_execpolicy_for_warnings;
use chaos_kern::config::Config;
use chaos_kern::config::ConfigOverrides;
use chaos_kern::config::load_config_as_toml_with_cli_overrides;
use chaos_kern::config::load_config_or_exit as kern_load_config_or_exit;
use chaos_kern::config_loader::ConfigLoadError;
use chaos_kern::config_loader::LoaderOverrides;
use chaos_kern::config_loader::format_config_error_with_source;
use chaos_kern::config_loader::format_error_chain;
use chaos_kern::find_process_id_by_name;
use chaos_kern::format_exec_policy_error_with_source;
use chaos_kern::models_manager::CollaborationModesConfig;
use chaos_kern::path_utils;
use chaos_kern::runtime_db::get_runtime_db;
use chaos_kern::terminal::Multiplexer;
use chaos_proc::RuntimeDbHandle;
use chaos_pwd::find_chaos_home;
use chaos_realpath::AbsolutePathBuf;
use chaos_snitch::BoxedLogLayer;
use chaos_snitch::open_debug_log_file_layer;
use chaos_snitch::open_log_file_layer;
use chaos_snitch::runtime_db;
use cwd_prompt::CwdPromptAction;
use cwd_prompt::CwdPromptOutcome;
use cwd_prompt::CwdSelection;
use std::path::Path;
use std::path::PathBuf;
use tracing::error;
use tracing::warn;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;
use uuid::Uuid;

mod additional_dirs;
mod app;
mod app_backtrack;
// mod ascii_animation; // removed: boot no longer shows animation
mod cli;
mod cwd_prompt;
mod external_editor;
mod file_search;
// mod frames; // removed: boot no longer shows animation
pub mod onboarding;
mod pager_overlay;
mod panes;
pub mod public_widgets;
mod resume_picker;
mod selection_list;
mod side_panel;
mod tile_manager;

// Modules now living in libui — re-exported under the same name so
// all existing `crate::foo` paths inside console continue to resolve.
use libui::app_event;
use libui::app_event_sender;
use libui::bottom_pane;
use libui::chatwidget;
pub use libui::custom_terminal;
pub use libui::debug_config;
use libui::diff_render;
#[cfg(test)]
use libui::exec_cell;
use libui::exec_command;
use libui::history_cell;
pub use libui::insert_history;
use libui::key_hint;
pub use libui::live_wrap;
use libui::markdown_render;
use libui::multi_agents;
use libui::render;
use libui::session_log;
use libui::shimmer;
use libui::style;
#[cfg(feature = "vt100-tests")]
pub use libui::test_backend;
use libui::text_formatting;
pub(crate) use libui::theme;
pub use libui::theme_picker;
use libui::tool_badges;
use libui::tui;

const DEFAULT_LOG_FILTER: &str = "chaos_kern=info,chaos_console=info,libui=warn,\
chaos_mcp_runtime=info,mcp_guest=info";

const DEBUG_LOG_FILTER: &str = "warn,chaos_kern=debug,chaos_coreboot=debug,chaos_boot=debug,chaos_fork=debug,\
chaos_console=debug,chaos_mcpd=debug,chaos_pam=debug,chaos_snitch=debug,\
chaos_ipc=debug,chaos_selinux=debug,chaos_dtrace=debug,chaos_halluacinate=debug,\
chaos_mcp_runtime=debug,mcp_guest=debug,chaos_clamp=debug,chaos_parrot=debug";

fn init_optional_debug_file_layer() -> std::io::Result<(
    Option<BoxedLogLayer<tracing_subscriber::Registry>>,
    Option<WorkerGuard>,
)> {
    open_debug_log_file_layer::<tracing_subscriber::Registry>(DEBUG_LOG_FILTER)
}

use crate::onboarding::onboarding_screen::OnboardingScreenArgs;
use crate::onboarding::onboarding_screen::run_onboarding_app;
use crate::tui::Tui;
use chaos_argv::Arg0DispatchPaths;
pub use cli::Cli;
pub use markdown_render::render_markdown_text;
pub use public_widgets::composer_input::ComposerAction;
pub use public_widgets::composer_input::ComposerInput;
// (tests access modules directly within the crate)

fn boot_core(config: &Config) -> CoreBoot {
    CoreBoot::boot(
        config,
        chaos_ipc::protocol::SessionSource::Cli,
        CollaborationModesConfig {
            default_mode_request_user_input: true,
        },
    )
}

pub async fn run_main(
    mut cli: Cli,
    arg0_paths: Arg0DispatchPaths,
    loader_overrides: LoaderOverrides,
) -> std::io::Result<AppExitInfo> {
    let (sandbox_mode, approval_policy) = if cli.auto_exec.headless {
        (
            Some(SandboxMode::RootAccess),
            Some(auto_exec_approval_policy()),
        )
    } else if cli.auto_exec.full_auto {
        (
            Some(SandboxMode::WorkspaceWrite),
            Some(auto_exec_approval_policy()),
        )
    } else {
        (
            cli.sandbox_mode.map(Into::<SandboxMode>::into),
            cli.approval_policy.map(Into::into),
        )
    };

    // Map the legacy --search flag to the canonical web_search mode.
    if cli.web_search {
        cli.config_overrides
            .raw_overrides
            .push("web_search=\"live\"".to_string());
    }

    // When using `--oss`, let the bootstrapper pick the model (defaulting to
    // gpt-oss:20b) and ensure it is present locally. Also, force the built‑in
    let raw_overrides = cli.config_overrides.raw_overrides.clone();
    // `oss` model provider.
    let overrides_cli = chaos_getopt::CliConfigOverrides { raw_overrides };
    let cli_kv_overrides = match overrides_cli.parse_overrides() {
        // Parse `-c` overrides from the CLI.
        Ok(v) => v,
        #[allow(clippy::print_stderr)]
        Err(e) => {
            eprintln!("Error parsing -c overrides: {e}");
            std::process::exit(1);
        }
    };

    // we load config.toml here to determine project state.
    #[allow(clippy::print_stderr)]
    let chaos_home = match find_chaos_home() {
        Ok(chaos_home) => chaos_home.to_path_buf(),
        Err(err) => {
            eprintln!("Error finding {OS_NAME} home: {err}");
            std::process::exit(1);
        }
    };

    let cwd = cli.cwd.clone();
    let config_cwd = match cwd.as_deref() {
        Some(path) => AbsolutePathBuf::from_absolute_path(path.canonicalize()?)?,
        None => AbsolutePathBuf::current_dir()?,
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
                eprintln!("Error loading config.toml: {}", format_error_chain(&err));
            }
            std::process::exit(1);
        }
    };

    if let Err(err) =
        chaos_kern::personality_migration::maybe_migrate_personality(&chaos_home, &config_toml)
            .await
    {
        tracing::warn!(error = %err, "failed to run personality migration");
    }

    // If `-c model_provider=<name>` was passed, promote it into the typed
    // ConfigOverrides so requirements.rs can detect it was explicitly overridden
    // (and avoid inheriting a model from a different provider).
    let cli_model_provider: Option<String> = cli_kv_overrides
        .iter()
        .find(|(k, _)| k == "model_provider")
        .and_then(|(_, v)| v.as_str().map(ToString::to_string));

    let additional_dirs = cli.add_dir.clone();

    let overrides = ConfigOverrides {
        approval_policy,
        sandbox_mode,
        cwd,
        provider_user_override: cli_model_provider.is_some(),
        model_provider: cli_model_provider,
        config_profile: cli.config_profile.clone(),
        alcatraz_exe: Some(arg0_paths.alcatraz_exe.clone()),
        additional_writable_roots: additional_dirs,
        ..Default::default()
    };

    let config = load_config_or_exit(cli_kv_overrides.clone(), overrides.clone()).await;

    chaos_kern::runtime_db::mount_vfs_for_startup(&config)
        .await
        .map_err(std::io::Error::other)?;

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

    if let Some(warning) =
        add_dir_warning_message(&cli.add_dir, config.permissions.sandbox_policy.get())
    {
        #[allow(clippy::print_stderr)]
        {
            eprintln!("Error adding directories: {warning}");
            std::process::exit(1);
        }
    }

    #[allow(clippy::print_stderr)]
    if let Err(err) = enforce_login_restrictions(&config) {
        eprintln!("{err}");
        std::process::exit(1);
    }

    let log_dir = chaos_kern::config::log_dir(&config)?;
    std::fs::create_dir_all(&log_dir)?;

    // Open (or create) the console log file, appending to it, with mode 0o600.
    // Keep target enabled so we can selectively filter via `RUST_LOG=...` and
    // grep for a specific module/target while troubleshooting.
    let (file_layer, _guard) = open_log_file_layer(
        &log_dir.join("chaos-console.log"),
        DEFAULT_LOG_FILTER,
        tracing_subscriber::fmt::format::FmtSpan::NEW
            | tracing_subscriber::fmt::format::FmtSpan::CLOSE,
    )?;
    let (debug_file_layer, _debug_log_guard) = init_optional_debug_file_layer()?;

    let otel = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        chaos_kern::otel_init::build_provider(
            &config,
            CHAOS_VERSION,
            /*service_name_override*/ None,
            /*default_analytics_enabled*/ true,
        )
    })) {
        Ok(Ok(otel)) => otel,
        Ok(Err(e)) => {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("Could not create otel exporter: {e}");
            }
            None
        }
        Err(_) => {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("Could not create otel exporter: panicked during initialization");
            }
            None
        }
    };

    let otel_logger_layer = otel.as_ref().and_then(|o| o.logger_layer());

    let otel_tracing_layer = otel.as_ref().and_then(|o| o.tracing_layer());

    let log_state_db = get_runtime_db(&config);
    let env_filter =
        || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    let log_db_layer = log_state_db
        .as_ref()
        .map(|db| runtime_db::start_runtime_db_layer(db.clone()).with_filter(env_filter()));

    let _ = tracing_subscriber::registry()
        .with(debug_file_layer)
        .with(file_layer)
        .with(log_db_layer)
        .with(otel_logger_layer)
        .with(otel_tracing_layer)
        .try_init();

    run_ratatui_app(
        cli,
        arg0_paths,
        loader_overrides,
        config,
        overrides,
        cli_kv_overrides,
        log_state_db,
    )
    .await
    .map_err(report_to_io_error)
}

fn report_to_io_error(report: color_eyre::eyre::Report) -> std::io::Error {
    let mut chain = report.chain();
    let mut message = chain
        .next()
        .map(ToString::to_string)
        .unwrap_or_else(|| report.to_string());
    let causes: Vec<String> = chain.map(ToString::to_string).collect();

    if !causes.is_empty() {
        message.push_str("\n\nCaused by:");
        for (index, cause) in causes.iter().enumerate() {
            message.push_str("\n    ");
            message.push_str(&index.to_string());
            message.push_str(": ");
            message.push_str(cause);
        }
    }

    std::io::Error::other(message)
}

#[allow(clippy::too_many_arguments)]
async fn run_ratatui_app(
    cli: Cli,
    _arg0_paths: Arg0DispatchPaths,
    _loader_overrides: LoaderOverrides,
    initial_config: Config,
    overrides: ConfigOverrides,
    cli_kv_overrides: Vec<(String, toml::Value)>,
    log_state_db: Option<RuntimeDbHandle>,
) -> color_eyre::Result<AppExitInfo> {
    color_eyre::install()?;

    // Forward panic reports through tracing so they appear in the UI status
    // line, but do not swallow the default/color-eyre panic handler.
    // Chain to the previous hook so users still get a rich panic report
    // (including backtraces) after we restore the terminal.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        prev_hook(info);
    }));
    let mut terminal = tui::init()?;
    terminal.clear()?;

    let mut tui = Tui::new(terminal);

    // Initialize high-fidelity session event logging if enabled.
    session_log::maybe_init(&initial_config);

    let should_show_trust_screen_flag = should_show_trust_screen(&initial_config);

    let config = if should_show_trust_screen_flag {
        let onboarding_result = run_onboarding_app(
            OnboardingScreenArgs {
                show_trust_screen: true,
                config: initial_config.clone(),
            },
            &mut tui,
        )
        .await?;
        if onboarding_result.should_exit {
            restore();
            session_log::log_session_end();
            let _ = tui.terminal.clear();
            return Ok(AppExitInfo {
                token_usage: chaos_ipc::protocol::TokenUsage::default(),
                process_id: None,
                process_name: None,
                exit_reason: ExitReason::UserRequested,
            });
        }

        // If the user made an explicit trust decision, reload config so current
        // process state reflects persisted trust changes.
        if onboarding_result.directory_trust_decision.is_some() {
            load_config_or_exit(cli_kv_overrides.clone(), overrides.clone()).await
        } else {
            initial_config
        }
    } else {
        initial_config
    };
    tui.set_terminal_title_enabled(
        config.terminal_title != chaos_kern::config::TerminalTitleMode::Off,
    );

    let mut missing_session_exit = |id_str: &str, action: &str| {
        error!("Error finding conversation path: {id_str}");
        restore();
        session_log::log_session_end();
        let _ = tui.terminal.clear();
        Ok(AppExitInfo {
            token_usage: chaos_ipc::protocol::TokenUsage::default(),
            process_id: None,
            process_name: None,
            exit_reason: ExitReason::Fatal(format!(
                "No saved session found with ID {id_str}. Run `chaos {action}` without an ID to choose from existing sessions."
            )),
        })
    };

    let use_fork = cli.fork_picker || cli.fork_last || cli.fork_session_id.is_some();
    let (action, session_id, last, picker, show_all) = if use_fork {
        (
            resume_picker::SessionPickerAction::Fork,
            cli.fork_session_id.as_deref(),
            cli.fork_last,
            cli.fork_picker,
            cli.fork_show_all,
        )
    } else {
        (
            resume_picker::SessionPickerAction::Resume,
            cli.resume_session_id.as_deref(),
            cli.resume_last,
            cli.resume_picker,
            cli.resume_show_all,
        )
    };
    let session_selection = if let Some(id_str) = session_id {
        match resolve_saved_process_id(id_str).await? {
            Some(process_id) => action.selection(process_id, false),
            None => return missing_session_exit(id_str, action.action_label()),
        }
    } else if last {
        let filter_cwd = if show_all {
            None
        } else {
            Some(config.cwd.as_path())
        };
        match RolloutRecorder::find_latest_process_id(
            &config,
            /*page_size*/ 1,
            /*cursor*/ None,
            ProcessSortKey::UpdatedAt,
            INTERACTIVE_SESSION_SOURCES,
            filter_cwd,
        )
        .await
        {
            Ok(Some(process_id)) => action.selection(process_id, false),
            Ok(None) => resume_picker::SessionSelection::StartFresh,
            Err(_) => resume_picker::SessionSelection::StartFresh,
        }
    } else if picker {
        resume_picker::run_session_picker(&mut tui, &config, show_all, action).await?
    } else {
        resume_picker::SessionSelection::StartFresh
    };
    if matches!(session_selection, resume_picker::SessionSelection::Exit) {
        restore();
        session_log::log_session_end();
        return Ok(AppExitInfo {
            token_usage: chaos_ipc::protocol::TokenUsage::default(),
            process_id: None,
            process_name: None,
            exit_reason: ExitReason::UserRequested,
        });
    }

    let current_cwd = config.cwd.clone();
    let allow_prompt = cli.cwd.is_none();
    let action_and_target_session_if_resume_or_fork = match &session_selection {
        resume_picker::SessionSelection::Resume(target_session) => {
            Some((CwdPromptAction::Resume, target_session))
        }
        resume_picker::SessionSelection::Fork(target_session) => {
            Some((CwdPromptAction::Fork, target_session))
        }
        _ => None,
    };
    let fallback_cwd = match action_and_target_session_if_resume_or_fork {
        Some((action, target_session)) => {
            match resolve_cwd_for_resume_or_fork(
                &mut tui,
                &config,
                &current_cwd,
                target_session.process_id,
                action,
                allow_prompt,
            )
            .await?
            {
                ResolveCwdOutcome::Continue(cwd) => cwd,
                ResolveCwdOutcome::Exit => {
                    restore();
                    session_log::log_session_end();
                    return Ok(AppExitInfo {
                        token_usage: chaos_ipc::protocol::TokenUsage::default(),
                        process_id: None,
                        process_name: None,
                        exit_reason: ExitReason::UserRequested,
                    });
                }
            }
        }
        None => None,
    };

    // Tab preserves the selection shown before choosing a destination, even if
    // loading that destination's cwd introduces different project defaults.
    let kept_selection = action_and_target_session_if_resume_or_fork
        .filter(|(_, target)| target.keep_current)
        .map(|_| {
            (
                config.model.clone(),
                config.model_provider_id.clone(),
                config.model_provider.clone(),
                config.model_reasoning_effort,
            )
        });
    let mut config = match &session_selection {
        resume_picker::SessionSelection::Resume(_) | resume_picker::SessionSelection::Fork(_) => {
            load_config_or_exit_with_fallback_cwd(
                cli_kv_overrides.clone(),
                overrides.clone(),
                fallback_cwd,
            )
            .await
        }
        _ => config,
    };
    if let Some((model, provider_id, provider, effort)) = kept_selection {
        config.model = model;
        config.model_provider_id = provider_id;
        config.model_provider = provider;
        config.model_reasoning_effort = effort;
    }

    if let Some((_, target)) = action_and_target_session_if_resume_or_fork
        && let Some(warning) = target
            .apply_saved_selection(&mut config, &cli_kv_overrides)
            .await?
    {
        config.startup_warnings.push(warning);
    }

    libui::theme::set_appearance(config.appearance.clone());

    // Configure syntax highlighting theme from the final config — onboarding
    // and resume/fork can both reload config with a different tui_theme, so
    // this must happen after the last possible reload.
    if let Some(w) = crate::render::highlight::set_theme_override(
        config.tui_theme.clone(),
        find_chaos_home().ok(),
    ) {
        config.startup_warnings.push(w);
    }

    let active_profile = config.active_profile.clone();
    let should_show_trust_screen = should_show_trust_screen(&config);
    let Cli {
        prompt,
        no_alt_screen,
        clamp: start_clamped,
        ..
    } = cli;

    config.clamp |= start_clamped;
    let start_clamped = config.clamp;
    let use_alt_screen = determine_alt_screen_mode(no_alt_screen, config.tui_alternate_screen);
    tui.set_alt_screen_enabled(use_alt_screen);
    let managers = boot_core(&config);

    let app_result = App::run(
        &mut tui,
        managers.auth_manager,
        managers.process_table,
        log_state_db,
        config,
        cli_kv_overrides.clone(),
        overrides.clone(),
        active_profile,
        prompt,
        Vec::new(),
        session_selection,
        should_show_trust_screen, // Proxy to: is it a first run in this directory?
        start_clamped,
    )
    .await;

    restore();
    // Mark the end of the recorded session.
    session_log::log_session_end();
    // ignore error when collecting usage – report underlying error instead
    app_result
}

async fn resolve_saved_process_id(id_str: &str) -> std::io::Result<Option<ProcessId>> {
    let process_id = if Uuid::parse_str(id_str).is_ok() {
        ProcessId::from_string(id_str).ok()
    } else {
        find_process_id_by_name(id_str).await?
    };
    let Some(process_id) = process_id else {
        return Ok(None);
    };
    if RolloutRecorder::journal_contains_process(process_id).await? {
        Ok(Some(process_id))
    } else {
        Ok(None)
    }
}

pub(crate) async fn read_session_cwd_by_process_id(
    config: &Config,
    process_id: ProcessId,
) -> Option<PathBuf> {
    if let Some(runtime_db_ctx) = get_runtime_db(config)
        && let Ok(Some(metadata)) = runtime_db_ctx.get_process(process_id).await
    {
        return Some(metadata.cwd);
    }

    match RolloutRecorder::read_process_cwd_from_journal(process_id).await {
        Ok(cwd) => cwd,
        Err(err) => {
            warn!(
                %process_id,
                %err,
                "Failed to read session cwd from journal"
            );
            None
        }
    }
}

pub(crate) fn cwds_differ(current_cwd: &Path, session_cwd: &Path) -> bool {
    match (
        path_utils::normalize_for_path_comparison(current_cwd),
        path_utils::normalize_for_path_comparison(session_cwd),
    ) {
        (Ok(current), Ok(session)) => current != session,
        _ => current_cwd != session_cwd,
    }
}

pub(crate) enum ResolveCwdOutcome {
    Continue(Option<PathBuf>),
    Exit,
}

pub(crate) async fn resolve_cwd_for_resume_or_fork(
    tui: &mut Tui,
    config: &Config,
    current_cwd: &Path,
    process_id: ProcessId,
    action: CwdPromptAction,
    allow_prompt: bool,
) -> color_eyre::Result<ResolveCwdOutcome> {
    let Some(history_cwd) = read_session_cwd_by_process_id(config, process_id).await else {
        return Ok(ResolveCwdOutcome::Continue(None));
    };
    if allow_prompt && cwds_differ(current_cwd, &history_cwd) {
        let selection_outcome =
            cwd_prompt::run_cwd_selection_prompt(tui, action, current_cwd, &history_cwd).await?;
        return Ok(match selection_outcome {
            CwdPromptOutcome::Selection(CwdSelection::Current) => {
                ResolveCwdOutcome::Continue(Some(current_cwd.to_path_buf()))
            }
            CwdPromptOutcome::Selection(CwdSelection::Session) => {
                ResolveCwdOutcome::Continue(Some(history_cwd))
            }
            CwdPromptOutcome::Exit => ResolveCwdOutcome::Exit,
        });
    }
    Ok(ResolveCwdOutcome::Continue(Some(history_cwd)))
}

#[expect(
    clippy::print_stderr,
    reason = "TUI should no longer be displayed, so we can write to stderr."
)]
fn restore() {
    if let Err(err) = tui::restore() {
        eprintln!(
            "failed to restore terminal. Run `reset` or restart your terminal to recover: {err}"
        );
    }
}

/// Determine whether to use the terminal's alternate screen buffer.
///
/// The alternate screen buffer provides a cleaner fullscreen experience without polluting
/// the terminal's scrollback history. However, it conflicts with terminal multiplexers like
/// Zellij that strictly follow the xterm spec, which disallows scrollback in alternate screen
/// buffers. Zellij intentionally disables scrollback in alternate screen mode (see
/// https://github.com/zellij-org/zellij/pull/1032) and offers no configuration option to
/// change this behavior.
///
/// This function implements a pragmatic workaround:
/// - If `--no-alt-screen` is explicitly passed, always disable alternate screen
/// - Otherwise, respect the `tui.alternate_screen` config setting:
///   - `always`: Use alternate screen everywhere (original behavior)
///   - `never`: Inline mode only, preserves scrollback
///   - `auto` (default): Auto-detect the terminal multiplexer and disable alternate screen
///     only in Zellij, enabling it everywhere else
fn determine_alt_screen_mode(no_alt_screen: bool, tui_alternate_screen: AltScreenMode) -> bool {
    if no_alt_screen {
        false
    } else {
        match tui_alternate_screen {
            AltScreenMode::Always => true,
            AltScreenMode::Never => false,
            AltScreenMode::Auto => {
                let terminal_info = chaos_kern::terminal::terminal_info();
                !matches!(terminal_info.multiplexer, Some(Multiplexer::Zellij { .. }))
            }
        }
    }
}

async fn load_config_or_exit(
    cli_kv_overrides: Vec<(String, toml::Value)>,
    overrides: ConfigOverrides,
) -> Config {
    kern_load_config_or_exit(cli_kv_overrides, overrides, None).await
}

async fn load_config_or_exit_with_fallback_cwd(
    cli_kv_overrides: Vec<(String, toml::Value)>,
    overrides: ConfigOverrides,
    fallback_cwd: Option<PathBuf>,
) -> Config {
    kern_load_config_or_exit(cli_kv_overrides, overrides, fallback_cwd).await
}

/// Determine if the user has decided whether to trust the current directory.
fn should_show_trust_screen(config: &Config) -> bool {
    config.active_project_trust.trust_level.is_none()
}

#[cfg(test)]
mod tests;
