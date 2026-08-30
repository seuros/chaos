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
use chaos_pam::XaiDeviceCode;
use chaos_pam::XaiDeviceCodeOptions;
use chaos_pam::complete_xai_device_code_login;
use chaos_pam::request_xai_device_code;
use chaos_pam::spawn_login_flow;
use codex_client::ChaosHttpClient;
use serde::Serialize;
use serde_json::Value;
use std::io::IsTerminal;
use std::io::Read;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use chaos_snitch::open_debug_log_file_layer;
use chaos_snitch::open_log_file_layer;

const CHATGPT_LOGIN_DISABLED_MESSAGE: &str =
    "ChatGPT account connection is disabled. Use an API key connection instead.";
const ACCOUNT_LOGIN_DISABLED_MESSAGE: &str =
    "Subscription account connection is disabled. Use an API key connection instead.";
const API_KEY_LOGIN_DISABLED_MESSAGE: &str =
    "API key connection is disabled. Use a ChatGPT account instead.";
const DEBUG_LOG_FILTER: &str = "warn,chaos_kern=debug,chaos_coreboot=debug,chaos_boot=debug,chaos_fork=debug,\
chaos_console=debug,chaos_mcpd=debug,chaos_pam=debug,chaos_snitch=debug,\
chaos_ipc=debug,chaos_selinux=debug,chaos_dtrace=debug,chaos_halluacinate=debug,\
mcp_guest=debug,chaos_clamp=debug,chaos_parrot=debug";
const OPENAI_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const XAI_USAGE_URL: &str = "https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig";

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

fn print_xai_device_code_prompt(device_code: &XaiDeviceCode) {
    eprintln!(
        concat!(
            "\nFollow these steps to sign in with xAI using device authorization:\n",
            "\n1. Open this link in your browser and sign in to your account\n   {}\n",
            "\n2. If prompted, enter this one-time code (expires in {} minutes)\n   {}\n",
            "\nDevice codes are a common phishing target. Never share this code.\n"
        ),
        device_code.verification_url,
        device_code.expires_in.div_ceil(60),
        device_code.user_code
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

/// Connect a supported subscription account using its OAuth device-code flow.
pub async fn run_connect_with_device_code(
    cli_config_overrides: CliConfigOverrides,
    issuer_base_url: Option<String>,
    client_id: Option<String>,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let _login_log_guards = init_accounts_file_logging(&config);
    tracing::info!(
        provider_id = %config.model_provider_id,
        "starting device code account connection flow"
    );
    if matches!(config.forced_login_method, Some(ForcedLoginMethod::Api)) {
        eprintln!("{ACCOUNT_LOGIN_DISABLED_MESSAGE}");
        std::process::exit(1);
    }
    if config
        .model_provider
        .supports_auth_method(ProviderAuthMethod::XaiAccount)
    {
        let mut opts =
            XaiDeviceCodeOptions::new(config.chaos_home, config.cli_auth_credentials_store_mode);
        if let Some(issuer) = issuer_base_url {
            opts.issuer = issuer;
        }
        if let Some(client_id) = client_id {
            opts.client_id = client_id;
        }
        let device_code = match request_xai_device_code(&opts).await {
            Ok(device_code) => device_code,
            Err(err) => {
                eprintln!("Error starting xAI device authorization: {err}");
                std::process::exit(1);
            }
        };
        print_xai_device_code_prompt(&device_code);
        match complete_xai_device_code_login(opts, device_code).await {
            Ok(()) => {
                eprintln!(
                    "Successfully connected {} using your xAI account",
                    config.model_provider.name
                );
                std::process::exit(0);
            }
            Err(err) => {
                eprintln!(
                    "Error connecting {} with device authorization: {err}",
                    config.model_provider.name
                );
                std::process::exit(1);
            }
        }
    }
    if !config
        .model_provider
        .supports_auth_method(ProviderAuthMethod::ChatgptAccount)
    {
        eprintln!(
            "{} does not support subscription account connections. Use `chaos --provider {} accounts --with-api-key` instead.",
            config.model_provider.name, config.model_provider_id
        );
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
        chaos_ipc::api::AuthMode::Xai => {
            let email = record
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.id_token.email.as_deref());
            match email {
                Some(email) => format!("{provider_name}: xAI account ({email})"),
                None => format!("{provider_name}: xAI account connected"),
            }
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
            AuthMode::Xai => {
                eprintln!("Active connection: {active_provider_name} xAI account")
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

#[derive(Debug, Serialize)]
struct AccountUsageSnapshot {
    provider: String,
    plan: Option<String>,
    windows: Vec<AccountUsageWindow>,
    observed_at: i64,
    source: &'static str,
}

#[derive(Debug, Serialize)]
struct AccountUsageWindow {
    id: String,
    label: String,
    used_percent: f64,
    resets_at: Option<i64>,
}

pub async fn run_accounts_usage(cli_config_overrides: CliConfigOverrides, json: bool) -> ! {
    if !json {
        eprintln!("`chaos accounts usage` is machine-readable; pass --json.");
        std::process::exit(2);
    }

    let config = load_config_or_exit(cli_config_overrides).await;
    let provider = config.model_provider_id.clone();
    if !matches!(provider.as_str(), "openai" | "xai") {
        eprintln!("Subscription usage is not supported for provider `{provider}`.");
        std::process::exit(2);
    }

    let manager = chaos_kern::AuthManager::shared(
        config.chaos_home.clone(),
        /*enable_codex_api_key_env*/ false,
        config.cli_auth_credentials_store_mode,
    )
    .for_provider(&provider);

    let result = fetch_account_usage(&provider, &manager).await;
    match result {
        Ok(snapshot) => match serde_json::to_string(&snapshot) {
            Ok(output) => {
                println!("{output}");
                std::process::exit(0);
            }
            Err(err) => {
                eprintln!("Failed to encode subscription usage: {err}");
                std::process::exit(1);
            }
        },
        Err(err) => {
            eprintln!("Failed to read {provider} subscription usage: {err}");
            std::process::exit(1);
        }
    }
}

async fn fetch_account_usage(
    provider: &str,
    manager: &std::sync::Arc<chaos_kern::AuthManager>,
) -> anyhow::Result<AccountUsageSnapshot> {
    let auth = manager
        .auth()
        .await
        .ok_or_else(|| anyhow::anyhow!("no connected subscription account"))?;
    if auth.is_api_key_auth() {
        anyhow::bail!("the selected provider is connected with an API key");
    }

    let first_token = auth.get_token()?;
    let account_id = auth.get_account_id();
    let first = request_account_usage(provider, &first_token, account_id.as_deref()).await;
    let payload = match first {
        Ok(payload) => payload,
        Err(AccountUsageFetchError::Unauthorized) if auth.is_managed_oauth_auth() => {
            manager
                .refresh_token()
                .await
                .map_err(|err| anyhow::anyhow!("provider authentication expired: {err}"))?;
            let refreshed = manager
                .auth()
                .await
                .ok_or_else(|| anyhow::anyhow!("provider authentication expired"))?;
            request_account_usage(
                provider,
                &refreshed.get_token()?,
                refreshed.get_account_id().as_deref(),
            )
            .await
            .map_err(anyhow::Error::from)?
        }
        Err(err) => return Err(err.into()),
    };

    match provider {
        "openai" => parse_openai_usage(&payload),
        "xai" => parse_xai_usage(&payload),
        _ => unreachable!("provider validated above"),
    }
}

#[derive(Debug)]
enum AccountUsagePayload {
    Json(Value),
    Bytes(Vec<u8>),
}

#[derive(Debug, thiserror::Error)]
enum AccountUsageFetchError {
    #[error("provider authentication expired")]
    Unauthorized,
    #[error("provider returned HTTP {0}")]
    Http(u16),
    #[error("provider returned an invalid response")]
    Invalid,
    #[error("provider request failed: {0}")]
    Request(String),
}

async fn request_account_usage(
    provider: &str,
    token: &str,
    account_id: Option<&str>,
) -> Result<AccountUsagePayload, AccountUsageFetchError> {
    let client = ChaosHttpClient::default_client();
    let response = if provider == "openai" {
        let mut request = client
            .get(OPENAI_USAGE_URL)
            .bearer_auth(token)
            .header("Accept", "application/json")
            .header("User-Agent", "chaos");
        if let Some(account_id) = account_id {
            request = request.header("ChatGPT-Account-Id", account_id);
        }
        request
            .send()
            .await
            .map_err(|err| AccountUsageFetchError::Request(err.to_string()))?
    } else {
        client
            .post(XAI_USAGE_URL)
            .bearer_auth(token)
            .header("Accept", "*/*")
            .header("Content-Type", "application/grpc-web+proto")
            .header("Origin", "https://grok.com")
            .header("Referer", "https://grok.com/?_s=usage")
            .header("x-grpc-web", "1")
            .header("x-user-agent", "connect-es/2.1.1")
            .header("User-Agent", "chaos")
            .body(vec![0_u8; 5])
            .send()
            .await
            .map_err(|err| AccountUsageFetchError::Request(err.to_string()))?
    };

    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(AccountUsageFetchError::Unauthorized);
    }
    if !response.status().is_success() {
        return Err(AccountUsageFetchError::Http(response.status().as_u16()));
    }

    if provider == "openai" {
        Ok(AccountUsagePayload::Json(response.json().await.map_err(
            |err| AccountUsageFetchError::Request(err.to_string()),
        )?))
    } else {
        let header_grpc_status = response
            .headers()
            .get("grpc-status")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u8>().ok());
        if header_grpc_status.is_some_and(|status| status != 0) {
            return Err(if header_grpc_status == Some(16) {
                AccountUsageFetchError::Unauthorized
            } else {
                AccountUsageFetchError::Invalid
            });
        }
        let body = response
            .bytes()
            .await
            .map_err(|err| AccountUsageFetchError::Request(err.to_string()))?
            .to_vec();
        let trailer_grpc_status = grpc_web_status(&body);
        if trailer_grpc_status.is_some_and(|status| status != 0) {
            return Err(if trailer_grpc_status == Some(16) {
                AccountUsageFetchError::Unauthorized
            } else {
                AccountUsageFetchError::Invalid
            });
        }
        Ok(AccountUsagePayload::Bytes(body))
    }
}

fn parse_openai_usage(payload: &AccountUsagePayload) -> anyhow::Result<AccountUsageSnapshot> {
    let AccountUsagePayload::Json(root) = payload else {
        anyhow::bail!("provider returned an invalid response");
    };
    let mut windows = Vec::new();
    if let Some(rate_limit) = root.get("rate_limit") {
        append_openai_window(
            &mut windows,
            "session",
            "Session",
            rate_limit.get("primary_window"),
        );
        append_openai_window(
            &mut windows,
            "weekly",
            "Weekly",
            rate_limit.get("secondary_window"),
        );
    }
    if let Some(additional) = root.get("additional_rate_limits").and_then(Value::as_array) {
        for (index, entry) in additional.iter().enumerate() {
            let label = entry
                .get("limit_name")
                .and_then(Value::as_str)
                .or_else(|| entry.get("metered_feature").and_then(Value::as_str))
                .unwrap_or("Additional limit");
            let id = slugify_usage_id(label, index);
            let rate_limit = entry.get("rate_limit");
            append_openai_window(
                &mut windows,
                &format!("{id}-session"),
                &format!("{label} session"),
                rate_limit.and_then(|value| value.get("primary_window")),
            );
            append_openai_window(
                &mut windows,
                &format!("{id}-weekly"),
                &format!("{label} weekly"),
                rate_limit.and_then(|value| value.get("secondary_window")),
            );
        }
    }
    if windows.is_empty() {
        anyhow::bail!("provider returned no subscription windows");
    }
    Ok(AccountUsageSnapshot {
        provider: "openai".to_string(),
        plan: root
            .get("plan_type")
            .and_then(Value::as_str)
            .map(str::to_string),
        windows,
        observed_at: unix_now(),
        source: "chatgpt-wham",
    })
}

fn append_openai_window(
    windows: &mut Vec<AccountUsageWindow>,
    id: &str,
    label: &str,
    value: Option<&Value>,
) {
    let Some(value) = value else { return };
    let Some(used_percent) = value.get("used_percent").and_then(Value::as_f64) else {
        return;
    };
    windows.push(AccountUsageWindow {
        id: id.to_string(),
        label: label.to_string(),
        used_percent: used_percent.clamp(0.0, 100.0),
        resets_at: value.get("reset_at").and_then(Value::as_i64),
    });
}

fn parse_xai_usage(payload: &AccountUsagePayload) -> anyhow::Result<AccountUsageSnapshot> {
    let AccountUsagePayload::Bytes(body) = payload else {
        anyhow::bail!("provider returned an invalid response");
    };
    let frames = grpc_web_data_frames(body);
    let data = frames
        .first()
        .map(Vec::as_slice)
        .ok_or_else(|| anyhow::anyhow!("provider returned an invalid response"))?;
    let mut fixed32 = Vec::new();
    let mut varints = Vec::new();
    scan_protobuf(data, &mut Vec::new(), 0, &mut fixed32, &mut varints);
    let used_percent = fixed32
        .iter()
        .filter(|(_, value)| value.is_finite() && *value >= 0.0 && *value <= 100.0)
        .map(|(_, value)| f64::from(*value))
        .next()
        // Proto3 omits scalar fields at their default value. Grok therefore
        // sends no fixed32 percentage at the start of a fresh usage period.
        .unwrap_or(0.0);
    let now = unix_now();
    let resets_at = varints
        .iter()
        .filter_map(|(path, value)| {
            let timestamp = i64::try_from(*value).ok()?;
            (timestamp > now && timestamp < 4_102_444_800).then_some((path, timestamp))
        })
        .min_by_key(|(path, timestamp)| (path.as_slice() != [1, 5, 1], *timestamp))
        .map(|(_, timestamp)| timestamp);

    Ok(AccountUsageSnapshot {
        provider: "xai".to_string(),
        plan: None,
        windows: vec![AccountUsageWindow {
            id: "subscription".to_string(),
            label: "Subscription".to_string(),
            used_percent,
            resets_at,
        }],
        observed_at: now,
        source: "grok-oauth",
    })
}

fn grpc_web_data_frames(data: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut index = 0;
    while index + 5 <= data.len() {
        let flags = data[index];
        let length = u32::from_be_bytes([
            data[index + 1],
            data[index + 2],
            data[index + 3],
            data[index + 4],
        ]) as usize;
        let start = index + 5;
        let Some(end) = start.checked_add(length) else {
            return Vec::new();
        };
        if end > data.len() {
            return Vec::new();
        }
        if flags & 0x80 == 0 {
            frames.push(data[start..end].to_vec());
        }
        index = end;
    }
    frames
}

fn grpc_web_status(data: &[u8]) -> Option<u8> {
    let mut index = 0;
    while index + 5 <= data.len() {
        let flags = data[index];
        let length = u32::from_be_bytes([
            data[index + 1],
            data[index + 2],
            data[index + 3],
            data[index + 4],
        ]) as usize;
        let start = index + 5;
        let end = start.checked_add(length)?;
        if end > data.len() {
            return None;
        }
        if flags & 0x80 != 0 {
            let trailers = std::str::from_utf8(&data[start..end]).ok()?;
            for line in trailers.lines() {
                if let Some(value) = line
                    .strip_prefix("grpc-status:")
                    .or_else(|| line.strip_prefix("Grpc-Status:"))
                {
                    return value.trim().parse().ok();
                }
            }
        }
        index = end;
    }
    None
}

fn scan_protobuf(
    data: &[u8],
    path: &mut Vec<u64>,
    depth: usize,
    fixed32: &mut Vec<(Vec<u64>, f32)>,
    varints: &mut Vec<(Vec<u64>, u64)>,
) {
    let mut index = 0;
    while index < data.len() {
        let field_start = index;
        let Some(key) = read_varint(data, &mut index) else {
            break;
        };
        if key == 0 {
            index = field_start + 1;
            continue;
        }
        let field = key >> 3;
        let wire = key & 7;
        path.push(field);
        match wire {
            0 => {
                if let Some(value) = read_varint(data, &mut index) {
                    varints.push((path.clone(), value));
                }
            }
            1 => index = index.saturating_add(8).min(data.len()),
            2 => {
                let Some(length) =
                    read_varint(data, &mut index).and_then(|value| usize::try_from(value).ok())
                else {
                    path.pop();
                    break;
                };
                let Some(end) = index.checked_add(length) else {
                    path.pop();
                    break;
                };
                if end > data.len() {
                    path.pop();
                    break;
                }
                if depth < 4 {
                    scan_protobuf(&data[index..end], path, depth + 1, fixed32, varints);
                }
                index = end;
            }
            5 if index + 4 <= data.len() => {
                let bits = u32::from_le_bytes([
                    data[index],
                    data[index + 1],
                    data[index + 2],
                    data[index + 3],
                ]);
                fixed32.push((path.clone(), f32::from_bits(bits)));
                index += 4;
            }
            _ => index = field_start + 1,
        }
        path.pop();
    }
}

fn read_varint(data: &[u8], index: &mut usize) -> Option<u64> {
    let mut value = 0_u64;
    let mut shift = 0_u32;
    while *index < data.len() && shift < 64 {
        let byte = data[*index];
        *index += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
    }
    None
}

fn slugify_usage_id(label: &str, fallback: usize) -> String {
    let slug = label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        format!("additional-{fallback}")
    } else {
        slug
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
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
    use super::{
        AccountUsagePayload, grpc_web_status, parse_openai_usage, parse_xai_usage, safe_format_key,
        unix_now,
    };
    use serde_json::json;

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

    #[test]
    fn parses_openai_subscription_windows() {
        let snapshot = parse_openai_usage(&AccountUsagePayload::Json(json!({
            "plan_type": "pro",
            "rate_limit": {
                "primary_window": { "used_percent": 80.0, "reset_at": 1_800_000_000 },
                "secondary_window": { "used_percent": 25.5, "reset_at": 1_800_100_000 }
            }
        })))
        .expect("usage should parse");

        assert_eq!(snapshot.provider, "openai");
        assert_eq!(snapshot.plan.as_deref(), Some("pro"));
        assert_eq!(snapshot.windows.len(), 2);
        assert_eq!(snapshot.windows[0].id, "session");
        assert_eq!(snapshot.windows[0].used_percent, 80.0);
        assert_eq!(snapshot.windows[1].id, "weekly");
        assert_eq!(snapshot.windows[1].used_percent, 25.5);
    }

    #[test]
    fn parses_xai_subscription_percentage_and_reset() {
        let reset_at = unix_now() + 3_600;
        let mut protobuf = vec![0x0d];
        protobuf.extend_from_slice(&100.0_f32.to_bits().to_le_bytes());
        protobuf.push(0x10);
        append_varint(&mut protobuf, reset_at as u64);

        let mut body = vec![0];
        body.extend_from_slice(&(protobuf.len() as u32).to_be_bytes());
        body.extend_from_slice(&protobuf);
        let snapshot =
            parse_xai_usage(&AccountUsagePayload::Bytes(body)).expect("usage should parse");

        assert_eq!(snapshot.provider, "xai");
        assert_eq!(snapshot.windows[0].used_percent, 100.0);
        assert_eq!(snapshot.windows[0].resets_at, Some(reset_at));
    }

    #[test]
    fn parses_omitted_xai_percentage_as_zero_usage() {
        let reset_at = unix_now() + 3_600;
        let mut timestamp = vec![0x08];
        append_varint(&mut timestamp, reset_at as u64);
        let mut protobuf = vec![0x0a, (timestamp.len() + 2) as u8, 0x2a, timestamp.len() as u8];
        protobuf.extend_from_slice(&timestamp);

        let mut body = vec![0];
        body.extend_from_slice(&(protobuf.len() as u32).to_be_bytes());
        body.extend_from_slice(&protobuf);
        let snapshot =
            parse_xai_usage(&AccountUsagePayload::Bytes(body)).expect("usage should parse");

        assert_eq!(snapshot.provider, "xai");
        assert_eq!(snapshot.windows[0].used_percent, 0.0);
        assert_eq!(snapshot.windows[0].resets_at, Some(reset_at));
    }

    #[test]
    fn reads_grpc_status_from_trailer_frame() {
        let trailers = b"grpc-status: 16\r\ngrpc-message: unauthenticated\r\n";
        let mut body = vec![0x80];
        body.extend_from_slice(&(trailers.len() as u32).to_be_bytes());
        body.extend_from_slice(trailers);

        assert_eq!(grpc_web_status(&body), Some(16));
    }

    fn append_varint(bytes: &mut Vec<u8>, mut value: u64) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if value == 0 {
                break;
            }
        }
    }
}
