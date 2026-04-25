//! CLI account-management commands and their direct-user observability surfaces.
//!
//! Direct `chaos accounts` uses a small file-backed tracing setup centered on
//! account-connection flows. The command keeps its stderr/browser UX and writes
//! account diagnostics to `chaos-accounts.log`, giving support a durable artifact
//! for one-shot CLI runs.

use chaos_getopt::CliConfigOverrides;
use chaos_ipc::config_types::ForcedLoginMethod;
use chaos_kern::auth::AuthMode;
use chaos_kern::auth::CLIENT_ID;
use chaos_kern::auth::ProviderAuthRecord;
use chaos_kern::auth::disconnect_all_provider_accounts;
use chaos_kern::auth::disconnect_provider_account;
use chaos_kern::auth::load_auth_dot_json;
use chaos_kern::auth::login_with_provider_api_key;
use chaos_kern::config::Config;
use chaos_kern::config::ConfigOverrides;
use chaos_kern::config::load_config_or_exit as kern_load_config_or_exit;
use chaos_kern::{ModelProviderInfo, ProviderAuthMethod};
use chaos_pam::DeviceCode;
use chaos_pam::LoginFlowMode;
use chaos_pam::LoginFlowUpdate;
use chaos_pam::ServerOptions;
use chaos_pam::spawn_login_flow;
use std::io::IsTerminal;
use std::io::Read;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use chaos_snitch::open_debug_log_file_layer;
use chaos_snitch::open_log_file_layer;

const CHATGPT_LOGIN_DISABLED_MESSAGE: &str =
    "ChatGPT account connection is disabled. Use an API key connection instead.";
const API_KEY_LOGIN_DISABLED_MESSAGE: &str =
    "API key connection is disabled. Use a ChatGPT account instead.";
const DEBUG_LOG_FILTER: &str = "warn,chaos_kern=debug,chaos_coreboot=debug,chaos_boot=debug,chaos_fork=debug,\
chaos_console=debug,chaos_mcpd=debug,chaos_pam=debug,chaos_syslog=debug,\
chaos_ipc=debug,chaos_selinux=debug,chaos_dtrace=debug,chaos_hallucinate=debug,\
mcp_guest=debug,chaos_clamp=debug,chaos_parrot=debug";

/// Installs file-backed tracing for direct `chaos accounts` flows.
///
/// The accounts command records account-connection diagnostics in
/// `chaos-accounts.log` while preserving its normal stderr/browser UX.
fn init_accounts_file_logging(config: &Config) -> Vec<WorkerGuard> {
    let log_dir = match chaos_kern::config::log_dir(config) {
        Ok(log_dir) => log_dir,
        Err(err) => {
            eprintln!("Warning: failed to resolve accounts log directory: {err}");
            return Vec::new();
        }
    };

    if let Err(err) = std::fs::create_dir_all(&log_dir) {
        eprintln!(
            "Warning: failed to create accounts log directory {}: {err}",
            log_dir.display()
        );
        return Vec::new();
    }

    let log_path = log_dir.join("chaos-accounts.log");

    // Persist account-connection diagnostics to a file so one-shot CLI runs leave
    // behind a supportable auth log.
    let (file_layer, file_guard) = match open_log_file_layer(
        &log_path,
        "chaos_coreboot=info,chaos_boot=info,chaos_kern=info,chaos_pam=info",
        tracing_subscriber::fmt::format::FmtSpan::NONE,
    ) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!(
                "Warning: failed to open accounts log file {}: {err}",
                log_path.display()
            );
            return Vec::new();
        }
    };

    let (debug_file_layer, debug_guard) =
        match open_debug_log_file_layer::<tracing_subscriber::Registry>(DEBUG_LOG_FILTER) {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("Warning: failed to open debug log file: {err}");
                (None, None)
            }
        };

    if let Err(err) = tracing_subscriber::registry()
        .with(debug_file_layer)
        .with(file_layer)
        .try_init()
    {
        eprintln!(
            "Warning: failed to initialize accounts log file {}: {err}",
            log_path.display()
        );
        return Vec::new();
    }

    let mut guards = vec![file_guard];
    if let Some(g) = debug_guard {
        guards.push(g);
    }
    guards
}

fn print_browser_sign_in_prompt(actual_port: u16, auth_url: &str) {
    eprintln!(
        "Starting local account sign-in server on http://localhost:{actual_port}.\nIf your browser did not open, navigate to this URL to authenticate:\n\n{auth_url}\n\nOn a remote or headless machine? Use `chaos accounts --device-auth` instead."
    );
}

fn print_device_code_prompt(device_code: &DeviceCode) {
    eprintln!(
        concat!(
            "\nFollow these steps to sign in with ChatGPT using device code authorization:\n",
            "\n1. Open this link in your browser and sign in to your account\n   {}\n",
            "\n2. Enter this one-time code (expires in 15 minutes)\n   {}\n",
            "\nDevice codes are a common phishing target. Never share this code.\n"
        ),
        device_code.verification_url, device_code.user_code
    );
}

async fn run_chatgpt_account_flow(opts: ServerOptions, mode: LoginFlowMode) -> std::io::Result<()> {
    let mut handle = spawn_login_flow(opts, mode);
    while let Some(update) = handle.recv().await {
        match update {
            LoginFlowUpdate::DeviceCodePending => {}
            LoginFlowUpdate::DeviceCodeUnsupported => {
                eprintln!("Device code sign-in is not enabled; falling back to browser sign-in.");
            }
            LoginFlowUpdate::BrowserOpened {
                actual_port,
                auth_url,
            } => {
                print_browser_sign_in_prompt(actual_port, &auth_url);
            }
            LoginFlowUpdate::DeviceCodeReady { device_code } => {
                print_device_code_prompt(&device_code);
            }
            LoginFlowUpdate::Succeeded { .. } => {
                return Ok(());
            }
            LoginFlowUpdate::Failed { message } => {
                return Err(std::io::Error::other(message));
            }
            LoginFlowUpdate::Cancelled => {
                return Err(std::io::Error::other(
                    "Account connection was not completed",
                ));
            }
        }
    }

    Err(std::io::Error::other(
        "Account connection flow ended unexpectedly",
    ))
}

pub async fn run_connect_with_chatgpt_account(cli_config_overrides: CliConfigOverrides) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let _login_log_guards = init_accounts_file_logging(&config);
    tracing::info!(
        provider_id = %config.model_provider_id,
        "starting browser account connection flow"
    );

    if matches!(config.forced_login_method, Some(ForcedLoginMethod::Api)) {
        eprintln!("{CHATGPT_LOGIN_DISABLED_MESSAGE}");
        std::process::exit(1);
    }
    if !config
        .model_provider
        .supports_auth_method(ProviderAuthMethod::ChatgptAccount)
    {
        eprintln!(
            "{} does not support ChatGPT account connections. Use `chaos --provider {} accounts --with-api-key` instead.",
            config.model_provider.name, config.model_provider_id
        );
        std::process::exit(1);
    }

    let forced_chatgpt_workspace_id = config.forced_chatgpt_workspace_id.clone();
    let provider_name = config.model_provider.name.clone();

    let opts = ServerOptions::new(
        config.chaos_home,
        CLIENT_ID.to_string(),
        forced_chatgpt_workspace_id,
        config.cli_auth_credentials_store_mode,
    );

    match run_chatgpt_account_flow(opts, LoginFlowMode::Browser).await {
        Ok(_) => {
            eprintln!("Successfully connected {provider_name} using your ChatGPT account");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("Error connecting {provider_name}: {e}");
            std::process::exit(1);
        }
    }
}

pub async fn run_connect_with_api_key(
    cli_config_overrides: CliConfigOverrides,
    api_key: String,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let _login_log_guards = init_accounts_file_logging(&config);
    tracing::info!(
        provider_id = %config.model_provider_id,
        "starting provider api key connection flow"
    );

    if matches!(config.forced_login_method, Some(ForcedLoginMethod::Chatgpt)) {
        eprintln!("{API_KEY_LOGIN_DISABLED_MESSAGE}");
        std::process::exit(1);
    }
    if !config
        .model_provider
        .supports_auth_method(ProviderAuthMethod::ApiKey)
    {
        eprintln!(
            "{} does not support API key connections. Use a ChatGPT account connection instead.",
            config.model_provider.name
        );
        std::process::exit(1);
    }

    let provider_name = config.model_provider.name.clone();
    match login_with_provider_api_key(
        &config.chaos_home,
        &config.model_provider_id,
        &api_key,
        config.cli_auth_credentials_store_mode,
    ) {
        Ok(_) => {
            eprintln!("Successfully connected {provider_name} with an API key");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("Error connecting {provider_name}: {e}");
            std::process::exit(1);
        }
    }
}

pub fn read_api_key_from_stdin() -> String {
    let mut stdin = std::io::stdin();

    if stdin.is_terminal() {
        eprintln!(
            "--with-api-key expects the API key on stdin. Try piping it, e.g. `printenv OPENAI_API_KEY | chaos accounts --with-api-key`."
        );
        std::process::exit(1);
    }

    eprintln!("Reading API key from stdin...");

    let mut buffer = String::new();
    if let Err(err) = stdin.read_to_string(&mut buffer) {
        eprintln!("Failed to read API key from stdin: {err}");
        std::process::exit(1);
    }

    let api_key = buffer.trim().to_string();
    if api_key.is_empty() {
        eprintln!("No API key provided via stdin.");
        std::process::exit(1);
    }

    api_key
}

/// Connect a ChatGPT account using the OAuth device-code flow.
pub async fn run_connect_with_device_code(
    cli_config_overrides: CliConfigOverrides,
    issuer_base_url: Option<String>,
    client_id: Option<String>,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let _login_log_guards = init_accounts_file_logging(&config);
    tracing::info!("starting device code account connection flow");
    if matches!(config.forced_login_method, Some(ForcedLoginMethod::Api)) {
        eprintln!("{CHATGPT_LOGIN_DISABLED_MESSAGE}");
        std::process::exit(1);
    }
    let forced_chatgpt_workspace_id = config.forced_chatgpt_workspace_id.clone();
    let mut opts = ServerOptions::new(
        config.chaos_home,
        client_id.unwrap_or(CLIENT_ID.to_string()),
        forced_chatgpt_workspace_id,
        config.cli_auth_credentials_store_mode,
    );
    if let Some(iss) = issuer_base_url {
        opts.issuer = iss;
    }
    match run_chatgpt_account_flow(
        opts,
        LoginFlowMode::DeviceCode {
            allow_browser_fallback: false,
        },
    )
    .await
    {
        Ok(()) => {
            eprintln!(
                "Successfully connected {} using your ChatGPT account",
                config.model_provider.name
            );
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!(
                "Error connecting {} with device code: {e}",
                config.model_provider.name
            );
            std::process::exit(1);
        }
    }
}

/// Prefers device-code sign-in (with `open_browser = false`) when headless environment is
/// detected, but keeps `chaos accounts` working in environments where device-code may be
/// disabled/feature-gated. If the device-code flow is unsupported, this falls back to starting
/// the local browser sign-in server.
pub async fn run_connect_with_device_code_fallback_to_browser(
    cli_config_overrides: CliConfigOverrides,
    issuer_base_url: Option<String>,
    client_id: Option<String>,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let _login_log_guards = init_accounts_file_logging(&config);
    tracing::info!("starting account connection flow with device code fallback");
    if matches!(config.forced_login_method, Some(ForcedLoginMethod::Api)) {
        eprintln!("{CHATGPT_LOGIN_DISABLED_MESSAGE}");
        std::process::exit(1);
    }

    let forced_chatgpt_workspace_id = config.forced_chatgpt_workspace_id.clone();
    let mut opts = ServerOptions::new(
        config.chaos_home,
        client_id.unwrap_or(CLIENT_ID.to_string()),
        forced_chatgpt_workspace_id,
        config.cli_auth_credentials_store_mode,
    );
    if let Some(iss) = issuer_base_url {
        opts.issuer = iss;
    }
    opts.open_browser = false;

    match run_chatgpt_account_flow(
        opts,
        LoginFlowMode::DeviceCode {
            allow_browser_fallback: true,
        },
    )
    .await
    {
        Ok(()) => {
            eprintln!(
                "Successfully connected {} using your ChatGPT account",
                config.model_provider.name
            );
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!(
                "Error connecting {} with device code: {e}",
                config.model_provider.name
            );
            std::process::exit(1);
        }
    }
}

fn provider_display_name<'a>(config: &'a Config, provider_id: &'a str) -> &'a str {
    config
        .model_providers
        .get(provider_id)
        .map(|provider| provider.name.as_str())
        .unwrap_or(provider_id)
}

fn describe_provider_record(
    provider_name: &str,
    _provider: Option<&ModelProviderInfo>,
    record: &ProviderAuthRecord,
) -> String {
    match record.resolved_mode() {
        chaos_ipc::api::AuthMode::ApiKey => format!("{provider_name}: API key connected"),
        chaos_ipc::api::AuthMode::Chatgpt => {
            let email = record
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.id_token.email.as_deref());
            match email {
                Some(email) => format!("{provider_name}: ChatGPT account ({email})"),
                _ => format!("{provider_name}: ChatGPT account connected"),
            }
        }
        chaos_ipc::api::AuthMode::ChatgptAuthTokens => {
            format!("{provider_name}: externally managed ChatGPT tokens connected")
        }
    }
}

fn connected_provider_records(
    config: &Config,
) -> std::io::Result<std::collections::BTreeMap<String, ProviderAuthRecord>> {
    let mut providers = std::collections::BTreeMap::new();
    for mode in [
        chaos_kern::auth::AuthCredentialsStoreMode::Ephemeral,
        config.cli_auth_credentials_store_mode,
    ] {
        if let Some(auth) = load_auth_dot_json(&config.chaos_home, mode)? {
            for (provider_id, record) in auth.normalized_provider_records() {
                providers.insert(provider_id, record);
            }
        }
        if mode == chaos_kern::auth::AuthCredentialsStoreMode::Ephemeral
            && config.cli_auth_credentials_store_mode
                == chaos_kern::auth::AuthCredentialsStoreMode::Ephemeral
        {
            break;
        }
    }
    Ok(providers)
}

pub async fn run_accounts_status(cli_config_overrides: CliConfigOverrides) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;

    let auth_manager = chaos_kern::AuthManager::new(
        config.chaos_home.clone(),
        /*enable_codex_api_key_env*/ true,
        config.cli_auth_credentials_store_mode,
    );
    let active_auth = auth_manager.auth_for_provider(&config.model_provider_id);
    let connected_providers = match connected_provider_records(&config) {
        Ok(records) => records,
        Err(err) => {
            eprintln!("Error checking account status: {err}");
            std::process::exit(1);
        }
    };

    let active_provider_name = provider_display_name(&config, &config.model_provider_id);
    eprintln!(
        "Active provider: {active_provider_name} ({})",
        config.model_provider_id
    );

    if let Some(auth) = active_auth {
        match auth.auth_mode() {
            AuthMode::ApiKey => match auth.get_token() {
                Ok(api_key) => eprintln!(
                    "Active connection: {} API key ({})",
                    active_provider_name,
                    safe_format_key(&api_key)
                ),
                Err(err) => {
                    eprintln!(
                        "Active connection: {active_provider_name} API key (unavailable: {err})"
                    )
                }
            },
            AuthMode::Chatgpt => {
                eprintln!("Active connection: {active_provider_name} ChatGPT account")
            }
        }
    } else {
        eprintln!("Active connection: none");
    }

    if connected_providers.is_empty() {
        eprintln!("Stored provider accounts: none");
        std::process::exit(1);
    }

    eprintln!("Stored provider accounts:");
    for (provider_id, record) in connected_providers {
        let provider = config.model_providers.get(&provider_id);
        let provider_name = provider
            .map(|provider| provider.name.as_str())
            .unwrap_or(provider_id.as_str());
        eprintln!(
            "  - {}",
            describe_provider_record(provider_name, provider, &record)
        );
    }
    std::process::exit(0);
}

pub async fn run_login_status(cli_config_overrides: CliConfigOverrides) -> ! {
    run_accounts_status(cli_config_overrides).await
}

pub async fn run_disconnect(cli_config_overrides: CliConfigOverrides, all: bool) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let removal_result = if all {
        disconnect_all_provider_accounts(&config.chaos_home, config.cli_auth_credentials_store_mode)
    } else {
        disconnect_provider_account(
            &config.chaos_home,
            &config.model_provider_id,
            config.cli_auth_credentials_store_mode,
        )
    };

    match removal_result {
        Ok(true) => {
            if all {
                eprintln!("Disconnected all stored provider accounts");
            } else {
                eprintln!(
                    "Disconnected stored credentials for {}",
                    provider_display_name(&config, &config.model_provider_id)
                );
            }
            std::process::exit(0);
        }
        Ok(false) => {
            if all {
                eprintln!("No stored provider accounts were connected");
            } else {
                eprintln!(
                    "No stored credentials found for {}",
                    provider_display_name(&config, &config.model_provider_id)
                );
            }
            std::process::exit(0);
        }
        Err(e) => {
            if all {
                eprintln!("Error disconnecting provider accounts: {e}");
            } else {
                eprintln!(
                    "Error disconnecting {}: {e}",
                    provider_display_name(&config, &config.model_provider_id)
                );
            }
            std::process::exit(1);
        }
    }
}

pub async fn run_logout(cli_config_overrides: CliConfigOverrides) -> ! {
    run_disconnect(cli_config_overrides, /*all*/ true).await
}

async fn load_config_or_exit(cli_config_overrides: CliConfigOverrides) -> Config {
    let cli_overrides = match cli_config_overrides.parse_overrides() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error parsing -c overrides: {e}");
            std::process::exit(1);
        }
    };
    kern_load_config_or_exit(cli_overrides, ConfigOverrides::default(), None).await
}

fn safe_format_key(key: &str) -> String {
    if key.len() <= 13 {
        return "***".to_string();
    }
    let prefix = &key[..8];
    let suffix = &key[key.len() - 5..];
    format!("{prefix}***{suffix}")
}

#[cfg(test)]
mod tests {
    use super::safe_format_key;

    #[test]
    fn formats_long_key() {
        let key = "sk-proj-1234567890ABCDE";
        assert_eq!(safe_format_key(key), "sk-proj-***ABCDE");
    }

    #[test]
    fn short_key_returns_stars() {
        let key = "sk-proj-12345";
        assert_eq!(safe_format_key(key), "***");
    }
}
