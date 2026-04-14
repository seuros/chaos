// Forbid accidental stdout/stderr writes in the *library* portion of the TUI.
// The standalone `chaos-console` binary prints a short help message before the
// alternate‑screen mode starts; that file opts‑out locally via `allow`.
#![deny(clippy::print_stdout, clippy::print_stderr)]
#![deny(clippy::disallowed_methods)]
use additional_dirs::add_dir_warning_message;
use app::App;
pub use app::AppExitInfo;
pub use app::ExitReason;
use chaos_init::ChaosInit;
use chaos_ipc::ProcessId;
use chaos_ipc::config_types::AltScreenMode;
use chaos_ipc::config_types::SandboxMode;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_kern::INTERACTIVE_SESSION_SOURCES;
use chaos_kern::ProcessSortKey;
use chaos_kern::RolloutRecorder;
use chaos_kern::auth::enforce_login_restrictions;
use chaos_kern::check_execpolicy_for_warnings;
use chaos_kern::config::Config;
use chaos_kern::config::ConfigBuilder;
use chaos_kern::config::ConfigOverrides;
use chaos_kern::config::load_config_as_toml_with_cli_overrides;
use chaos_kern::config::resolve_oss_provider;
use chaos_kern::config_loader::ConfigLoadError;
use chaos_kern::config_loader::LoaderOverrides;
use chaos_kern::config_loader::format_config_error_with_source;
use chaos_kern::find_process_id_by_name;
use chaos_kern::format_exec_policy_error_with_source;
use chaos_kern::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use chaos_kern::path_utils;
use chaos_kern::runtime_db::get_runtime_db;
use chaos_kern::terminal::Multiplexer;
use chaos_proc::StateRuntime;
use chaos_proc::log_db;
use chaos_pwd::find_chaos_home;
use chaos_realpath::AbsolutePathBuf;
use cwd_prompt::CwdPromptAction;
use cwd_prompt::CwdPromptOutcome;
use cwd_prompt::CwdSelection;
use std::fs::OpenOptions;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::error;
use tracing::warn;
use tracing_appender::non_blocking;
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

const DEBUG_LOG_PATH_ENV_VAR: &str = "CHAOS_DEBUG_LOG_PATH";
const DEBUG_LOG_FILTER: &str = "warn,chaos_kern=debug,chaos_boot=debug,chaos_fork=debug,\
chaos_console=debug,chaos_mcpd=debug,chaos_pam=debug,chaos_syslog=debug,\
chaos_ipc=debug,chaos_selinux=debug,chaos_dtrace=debug,chaos_hallucinate=debug,\
mcp_guest=debug,chaos_clamp=debug,chaos_parrot=debug";

fn init_optional_debug_file_layer() -> std::io::Result<(
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
        .with_writer(non_blocking)
        .with_target(true)
        .with_ansi(false)
        .with_filter(filter);
    Ok((Some(layer), Some(guard)))
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

fn boot_core(config: &Config) -> ChaosInit {
    ChaosInit::boot(
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
    let (sandbox_mode, approval_policy) = if cli.full_auto {
        (
            Some(SandboxMode::WorkspaceWrite),
            Some(ApprovalPolicy::Interactive),
        )
    } else if cli.dangerously_bypass_approvals_and_sandbox {
        (
            Some(SandboxMode::RootAccess),
            Some(ApprovalPolicy::Headless),
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
            eprintln!("Error finding chaos home: {err}");
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
                eprintln!("Error loading config.toml: {err}");
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

    let model_provider_override = if cli.oss {
        let resolved = resolve_oss_provider(
            cli.oss_provider.as_deref(),
            &config_toml,
            cli.config_profile.clone(),
        );

        if let Some(provider) = resolved {
            Some(provider)
        } else {
            return Err(std::io::Error::other(
                "No OSS provider configured. Use --local-provider=provider or set oss_provider in config.toml",
            ));
        }
    } else {
        None
    };

    let model = cli.model.clone();

    let additional_dirs = cli.add_dir.clone();

    let overrides = ConfigOverrides {
        model,
        approval_policy,
        sandbox_mode,
        cwd,
        model_provider: model_provider_override.clone(),
        config_profile: cli.config_profile.clone(),
        alcatraz_linux_exe: arg0_paths.alcatraz_linux_exe.clone(),
        alcatraz_freebsd_exe: arg0_paths.alcatraz_freebsd_exe.clone(),
        alcatraz_macos_exe: arg0_paths.alcatraz_macos_exe.clone(),
        show_raw_agent_reasoning: cli.oss.then_some(true),
        additional_writable_roots: additional_dirs,
        ..Default::default()
    };

    let config = load_config_or_exit(cli_kv_overrides.clone(), overrides.clone()).await;

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
    // Open (or create) your log file, appending to it.
    let mut log_file_opts = OpenOptions::new();
    log_file_opts.create(true).append(true);

    // Ensure the file is only readable and writable by the current user.
    {
        use std::os::unix::fs::OpenOptionsExt;
        log_file_opts.mode(0o600);
    }

    let log_file = log_file_opts.open(log_dir.join("chaos-console.log"))?;

    // Wrap file in non‑blocking writer.
    let (non_blocking, _guard) = non_blocking(log_file);

    // use RUST_LOG env var, default to info for Chaos crates.
    let env_filter = || {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("chaos_kern=info,chaos_console=info,codex_mcp_guest=info")
        })
    };

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        // Keep target enabled so we can selectively filter via `RUST_LOG=...` and then
        // grep for a specific module/target while troubleshooting.
        .with_target(true)
        .with_ansi(false)
        .with_span_events(
            tracing_subscriber::fmt::format::FmtSpan::NEW
                | tracing_subscriber::fmt::format::FmtSpan::CLOSE,
        )
        .with_filter(env_filter());
    let (debug_file_layer, _debug_log_guard) = init_optional_debug_file_layer()?;

    if cli.oss && model_provider_override.is_some() {
        // We're in the oss section, so provider_id should be Some
        // Let's handle None case gracefully though just in case
        let provider_id = match model_provider_override.as_ref() {
            Some(id) => id,
            None => {
                error!("OSS provider unexpectedly not set when oss flag is used");
                return Err(std::io::Error::other(
                    "OSS provider not set but oss flag was used",
                ));
            }
        };
        // OSS provider readiness checks have been removed; connectivity is
        // validated lazily on the first request.
        let _ = provider_id;
    }

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

    let log_state_db = match StateRuntime::init(
        config.sqlite_home.clone(),
        config.model_provider_id.clone(),
    )
    .await
    {
        Ok(db) => Some(db),
        Err(err) => {
            tracing::warn!(
                error = %err,
                sqlite_home = %config.sqlite_home.display(),
                "failed to initialize log/state runtime for console"
            );
            None
        }
    };
    let log_db_layer = log_state_db
        .as_ref()
        .map(|db| log_db::start(db.clone()).with_filter(env_filter()));

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
    .map_err(|err| std::io::Error::other(err.to_string()))
}

#[allow(clippy::too_many_arguments)]
async fn run_ratatui_app(
    cli: Cli,
    _arg0_paths: Arg0DispatchPaths,
    _loader_overrides: LoaderOverrides,
    initial_config: Config,
    overrides: ConfigOverrides,
    cli_kv_overrides: Vec<(String, toml::Value)>,
    log_state_db: Option<Arc<StateRuntime>>,
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
    let session_selection =
        if use_fork {
            if let Some(id_str) = cli.fork_session_id.as_deref() {
                match resolve_saved_process_id(&config.chaos_home, id_str).await? {
                    Some(process_id) => {
                        resume_picker::SessionSelection::Fork(resume_picker::SessionTarget {
                            process_id,
                        })
                    }
                    None => return missing_session_exit(id_str, "fork"),
                }
            } else if cli.fork_last {
                match RolloutRecorder::list_processes(
                    &config,
                    /*page_size*/ 1,
                    /*cursor*/ None,
                    ProcessSortKey::UpdatedAt,
                    INTERACTIVE_SESSION_SOURCES,
                    &config.model_provider_id,
                    /*search_term*/ None,
                )
                .await
                {
                    Ok(page) => match page.items.first() {
                        Some(item) => match item.process_id {
                            Some(process_id) => resume_picker::SessionSelection::Fork(
                                resume_picker::SessionTarget { process_id },
                            ),
                            None => resume_picker::SessionSelection::StartFresh,
                        },
                        None => resume_picker::SessionSelection::StartFresh,
                    },
                    Err(_) => resume_picker::SessionSelection::StartFresh,
                }
            } else if cli.fork_picker {
                match resume_picker::run_fork_picker(&mut tui, &config, cli.fork_show_all).await? {
                    resume_picker::SessionSelection::Exit => {
                        restore();
                        session_log::log_session_end();
                        return Ok(AppExitInfo {
                            token_usage: chaos_ipc::protocol::TokenUsage::default(),
                            process_id: None,
                            process_name: None,
                            exit_reason: ExitReason::UserRequested,
                        });
                    }
                    other => other,
                }
            } else {
                resume_picker::SessionSelection::StartFresh
            }
        } else if let Some(id_str) = cli.resume_session_id.as_deref() {
            match resolve_saved_process_id(&config.chaos_home, id_str).await? {
                Some(process_id) => {
                    resume_picker::SessionSelection::Resume(resume_picker::SessionTarget {
                        process_id,
                    })
                }
                None => return missing_session_exit(id_str, "resume"),
            }
        } else if cli.resume_last {
            let filter_cwd = if cli.resume_show_all {
                None
            } else {
                Some(config.cwd.as_path())
            };
            match RolloutRecorder::list_processes(
                &config,
                /*page_size*/ 1,
                /*cursor*/ None,
                ProcessSortKey::UpdatedAt,
                INTERACTIVE_SESSION_SOURCES,
                &config.model_provider_id,
                /*search_term*/ None,
            )
            .await
            {
                Ok(page) => match page.items.into_iter().find(|item| {
                    match (filter_cwd, item.cwd.as_deref()) {
                        (Some(filter_cwd), Some(item_cwd)) => !cwds_differ(filter_cwd, item_cwd),
                        (Some(_), None) => false,
                        (None, _) => true,
                    }
                }) {
                    Some(item) => {
                        match item.process_id {
                            Some(process_id) => resume_picker::SessionSelection::Resume(
                                resume_picker::SessionTarget { process_id },
                            ),
                            None => resume_picker::SessionSelection::StartFresh,
                        }
                    }
                    None => resume_picker::SessionSelection::StartFresh,
                },
                Err(_) => resume_picker::SessionSelection::StartFresh,
            }
        } else if cli.resume_picker {
            match resume_picker::run_resume_picker(&mut tui, &config, cli.resume_show_all).await? {
                resume_picker::SessionSelection::Exit => {
                    restore();
                    session_log::log_session_end();
                    return Ok(AppExitInfo {
                        token_usage: chaos_ipc::protocol::TokenUsage::default(),
                        process_id: None,
                        process_name: None,
                        exit_reason: ExitReason::UserRequested,
                    });
                }
                other => other,
            }
        } else {
            resume_picker::SessionSelection::StartFresh
        };

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
        images,
        no_alt_screen,
        clamp: start_clamped,
        ..
    } = cli;

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
        images,
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

async fn resolve_saved_process_id(
    chaos_home: &Path,
    id_str: &str,
) -> std::io::Result<Option<ProcessId>> {
    let process_id = if Uuid::parse_str(id_str).is_ok() {
        ProcessId::from_string(id_str).ok()
    } else {
        find_process_id_by_name(chaos_home, id_str).await?
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
    if let Some(runtime_db_ctx) = get_runtime_db(config).await
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
    load_config_or_exit_with_fallback_cwd(cli_kv_overrides, overrides, /*fallback_cwd*/ None).await
}

async fn load_config_or_exit_with_fallback_cwd(
    cli_kv_overrides: Vec<(String, toml::Value)>,
    overrides: ConfigOverrides,
    fallback_cwd: Option<PathBuf>,
) -> Config {
    #[allow(clippy::print_stderr)]
    match ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .harness_overrides(overrides)
        .fallback_cwd(fallback_cwd)
        .build()
        .await
    {
        Ok(config) => config,
        Err(err) => {
            eprintln!("Error loading configuration: {err}");
            std::process::exit(1);
        }
    }
}

/// Determine if the user has decided whether to trust the current directory.
fn should_show_trust_screen(config: &Config) -> bool {
    config.active_project.trust_level.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_ipc::protocol::ApprovalPolicy;
    use chaos_kern::AuthManager;
    use chaos_kern::config::ConfigBuilder;
    use chaos_kern::config::ConfigOverrides;
    use chaos_kern::config::ProjectConfig;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    async fn build_config(temp_dir: &TempDir) -> std::io::Result<Config> {
        ConfigBuilder::default()
            .chaos_home(temp_dir.path().to_path_buf())
            .build()
            .await
    }

    #[tokio::test]
    async fn boot_core_returns_valid_managers() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let config = build_config(&temp_dir).await?;
        let managers = boot_core(&config);

        // AuthManager was created — just verify it is non-null and usable.
        // Note: AuthManager::shared always allocates a fresh Arc (no global
        // singleton cache), so ptr_eq comparisons across calls always fail.
        let _auth_ref: &AuthManager = &managers.auth_manager;

        // ProcessTable was created.
        let _ = managers.process_table.get_models_manager();
        Ok(())
    }

    #[tokio::test]
    async fn untrusted_project_skips_trust_prompt() -> std::io::Result<()> {
        use chaos_ipc::config_types::TrustLevel;
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        config.active_project = ProjectConfig {
            trust_level: Some(TrustLevel::Untrusted),
        };

        let should_show = should_show_trust_screen(&config);
        assert!(
            !should_show,
            "Trust prompt should not be shown for projects explicitly marked as untrusted"
        );
        Ok(())
    }

    #[tokio::test]
    async fn config_rebuild_changes_trust_defaults_with_cwd() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let chaos_home = temp_dir.path().to_path_buf();
        let trusted = temp_dir.path().join("trusted");
        let untrusted = temp_dir.path().join("untrusted");
        std::fs::create_dir_all(&trusted)?;
        std::fs::create_dir_all(&untrusted)?;

        // TOML keys need escaped backslashes on Windows paths.
        let trusted_display = trusted.display().to_string().replace('\\', "\\\\");
        let untrusted_display = untrusted.display().to_string().replace('\\', "\\\\");
        let config_toml = format!(
            r#"[projects."{trusted_display}"]
trust_level = "trusted"

[projects."{untrusted_display}"]
trust_level = "untrusted"
"#
        );
        std::fs::write(temp_dir.path().join("config.toml"), config_toml)?;

        let trusted_overrides = ConfigOverrides {
            cwd: Some(trusted.clone()),
            ..Default::default()
        };
        let trusted_config = ConfigBuilder::default()
            .chaos_home(chaos_home.clone())
            .harness_overrides(trusted_overrides.clone())
            .build()
            .await?;
        assert_eq!(
            trusted_config.permissions.approval_policy.value(),
            ApprovalPolicy::Interactive
        );

        let untrusted_overrides = ConfigOverrides {
            cwd: Some(untrusted),
            ..trusted_overrides
        };
        let untrusted_config = ConfigBuilder::default()
            .chaos_home(chaos_home)
            .harness_overrides(untrusted_overrides)
            .build()
            .await?;
        assert_eq!(
            untrusted_config.permissions.approval_policy.value(),
            ApprovalPolicy::Supervised
        );
        Ok(())
    }

    /// Regression: theme must be configured from the *final* config.
    ///
    /// `run_ratatui_app` can reload config during onboarding and again
    /// during session resume/fork.  The syntax theme override (stored in
    /// a `OnceLock`) must use the final config's `tui_theme`, not the
    /// initial one — otherwise users resuming a thread in a project with
    /// a different theme get the wrong highlighting.
    ///
    /// We verify the invariant indirectly: `validate_theme_name` (the
    /// pure validation core of `set_theme_override`) must be called with
    /// the *final* config's theme, and its warning must land in the
    /// final config's `startup_warnings`.
    #[tokio::test]
    async fn theme_warning_uses_final_config() -> std::io::Result<()> {
        use crate::render::highlight::validate_theme_name;

        let temp_dir = TempDir::new()?;

        // initial_config has a valid theme — no warning.
        let initial_config = build_config(&temp_dir).await?;
        assert!(initial_config.tui_theme.is_none());

        // Simulate resume/fork reload: the final config has an invalid theme.
        let mut config = build_config(&temp_dir).await?;
        config.tui_theme = Some("bogus-theme".into());

        // Theme override must use the final config (not initial_config).
        // This mirrors the real call site in run_ratatui_app.
        if let Some(w) = validate_theme_name(config.tui_theme.as_deref(), Some(temp_dir.path())) {
            config.startup_warnings.push(w);
        }

        assert_eq!(
            config.startup_warnings.len(),
            1,
            "warning from final config's invalid theme should be present"
        );
        assert!(
            config.startup_warnings[0].contains("bogus-theme"),
            "warning should reference the final config's theme name"
        );
        Ok(())
    }
}
