use chaos_kern::auth::AuthCredentialsStoreMode;
use chaos_kern::auth::login_with_xai_oauth_tokens;
use codex_client::ChaosHttpClient;
use serde::Deserialize;
use std::io;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use url::Url;

pub const XAI_OAUTH_ISSUER: &str = "https://auth.x.ai";
pub const XAI_OAUTH_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const XAI_OAUTH_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access \
conversations:read conversations:write workspaces:read workspaces:write";
const XAI_OAUTH_REFERRER: &str = "chaos";
const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;

#[derive(Debug, Clone)]
pub struct XaiDeviceCodeOptions {
    pub chaos_home: PathBuf,
    pub issuer: String,
    pub client_id: String,
    pub auth_credentials_store_mode: AuthCredentialsStoreMode,
}

impl XaiDeviceCodeOptions {
    pub fn new(chaos_home: PathBuf, auth_credentials_store_mode: AuthCredentialsStoreMode) -> Self {
        Self {
            chaos_home,
            issuer: XAI_OAUTH_ISSUER.to_string(),
            client_id: XAI_OAUTH_CLIENT_ID.to_string(),
            auth_credentials_store_mode,
        }
    }
}

#[derive(Debug, Clone)]
pub struct XaiDeviceCode {
    pub verification_url: String,
    pub user_code: String,
    pub expires_in: u64,
    device_code: String,
    interval: u64,
    token_endpoint: String,
}

#[derive(Debug, Deserialize)]
struct DiscoveryDocument {
    token_endpoint: String,
    device_authorization_endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorResponse {
    error: Option<String>,
    error_description: Option<String>,
}

fn form_body(fields: &[(&str, &str)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in fields {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

fn validated_endpoint(issuer: &str, endpoint: &str, field: &str) -> io::Result<String> {
    let issuer = Url::parse(issuer).map_err(io::Error::other)?;
    let endpoint = Url::parse(endpoint).map_err(io::Error::other)?;
    if issuer.scheme() != endpoint.scheme()
        || issuer.host_str() != endpoint.host_str()
        || issuer.port_or_known_default() != endpoint.port_or_known_default()
    {
        return Err(io::Error::other(format!(
            "xAI OAuth discovery returned {field} on a different origin"
        )));
    }
    if issuer.scheme() == "https" && endpoint.scheme() != "https" {
        return Err(io::Error::other(format!(
            "xAI OAuth discovery returned a non-HTTPS {field}"
        )));
    }
    Ok(endpoint.to_string())
}

fn validated_verification_url(issuer: &str, verification_url: &str) -> io::Result<String> {
    let issuer = Url::parse(issuer).map_err(io::Error::other)?;
    let verification_url = Url::parse(verification_url).map_err(io::Error::other)?;
    let production_accounts_app = issuer.host_str() == Some("auth.x.ai")
        && verification_url.host_str() == Some("accounts.x.ai")
        && verification_url.scheme() == "https";
    let same_origin = issuer.scheme() == verification_url.scheme()
        && issuer.host_str() == verification_url.host_str()
        && issuer.port_or_known_default() == verification_url.port_or_known_default();
    if !production_accounts_app && !same_origin {
        return Err(io::Error::other(
            "xAI device authorization returned a verification URL on an unexpected origin",
        ));
    }
    if issuer.scheme() == "https" && verification_url.scheme() != "https" {
        return Err(io::Error::other(
            "xAI device authorization returned a non-HTTPS verification URL",
        ));
    }
    Ok(verification_url.to_string())
}

async fn discover(opts: &XaiDeviceCodeOptions) -> io::Result<DiscoveryDocument> {
    let issuer = opts.issuer.trim_end_matches('/');
    let url = format!("{issuer}/.well-known/openid-configuration");
    let response = ChaosHttpClient::default_client()
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(io::Error::other)?;
    if !response.status().is_success() {
        return Err(io::Error::other(format!(
            "xAI OIDC discovery failed with status {}",
            response.status()
        )));
    }
    let mut discovery: DiscoveryDocument = response.json().await.map_err(io::Error::other)?;
    discovery.token_endpoint =
        validated_endpoint(issuer, &discovery.token_endpoint, "token endpoint")?;
    if let Some(endpoint) = discovery.device_authorization_endpoint.as_deref() {
        discovery.device_authorization_endpoint = Some(validated_endpoint(
            issuer,
            endpoint,
            "device authorization endpoint",
        )?);
    }
    Ok(discovery)
}

pub async fn request_xai_device_code(opts: &XaiDeviceCodeOptions) -> io::Result<XaiDeviceCode> {
    let discovery = discover(opts).await?;
    let endpoint = discovery.device_authorization_endpoint.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "xAI OIDC discovery did not advertise device authorization",
        )
    })?;
    let body = form_body(&[
        ("client_id", &opts.client_id),
        ("scope", XAI_OAUTH_SCOPE),
        ("referrer", XAI_OAUTH_REFERRER),
    ]);
    let response = ChaosHttpClient::default_client()
        .post(&endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(body)
        .send()
        .await
        .map_err(io::Error::other)?;
    if !response.status().is_success() {
        return Err(io::Error::other(format!(
            "xAI device code request failed with status {}",
            response.status()
        )));
    }
    let response: DeviceCodeResponse = response.json().await.map_err(io::Error::other)?;
    if response.device_code.is_empty() {
        return Err(io::Error::other(
            "xAI device authorization returned an empty device code",
        ));
    }
    if response.user_code.is_empty()
        || !response
            .user_code
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err(io::Error::other(
            "xAI device authorization returned an invalid user-code format",
        ));
    }
    Ok(XaiDeviceCode {
        // Keep the one-time code out of the URL. xAI also returns a
        // verification_uri_complete value with the code embedded, but callers
        // such as hosted-agent shims may safely log or display this plain URL
        // while transporting the user code through a separate secret field.
        verification_url: validated_verification_url(&opts.issuer, &response.verification_uri)?,
        user_code: response.user_code,
        expires_in: response.expires_in,
        device_code: response.device_code,
        interval: response
            .interval
            .unwrap_or(DEFAULT_POLL_INTERVAL_SECS)
            .max(1),
        token_endpoint: discovery.token_endpoint,
    })
}

pub async fn complete_xai_device_code_login(
    opts: XaiDeviceCodeOptions,
    device_code: XaiDeviceCode,
) -> io::Result<()> {
    let client = ChaosHttpClient::default_client();
    let started = Instant::now();
    let max_wait = Duration::from_secs(device_code.expires_in);
    let mut poll_interval = device_code.interval;

    let tokens = loop {
        let remaining = max_wait.saturating_sub(started.elapsed());
        tokio::time::sleep(Duration::from_secs(poll_interval).min(remaining)).await;
        if started.elapsed() >= max_wait {
            return Err(io::Error::other(
                "xAI device authorization expired before it was completed",
            ));
        }

        let body = form_body(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", &opts.client_id),
            ("device_code", &device_code.device_code),
        ]);
        let response = client
            .post(&device_code.token_endpoint)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .body(body)
            .send()
            .await
            .map_err(io::Error::other)?;
        let status = response.status();
        if status.is_success() {
            let tokens = response
                .json::<TokenResponse>()
                .await
                .map_err(io::Error::other)?;
            if tokens.access_token.is_empty() || tokens.refresh_token.is_empty() {
                return Err(io::Error::other(
                    "xAI device token exchange returned incomplete credentials",
                ));
            }
            break tokens;
        }

        let body = response.text().await.unwrap_or_default();
        let error = serde_json::from_str::<OAuthErrorResponse>(&body).ok();
        match error.as_ref().and_then(|error| error.error.as_deref()) {
            Some("authorization_pending") => {}
            Some("slow_down") => poll_interval = (poll_interval + 5).min(30),
            Some("access_denied" | "expired_token") => {
                let message = error
                    .and_then(|error| error.error_description.or(error.error))
                    .unwrap_or_else(|| "xAI device authorization was denied".to_string());
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, message));
            }
            _ => {
                return Err(io::Error::other(format!(
                    "xAI device token exchange failed with status {status}"
                )));
            }
        }
    };

    login_with_xai_oauth_tokens(
        &opts.chaos_home,
        tokens.id_token.as_deref(),
        &tokens.access_token,
        &tokens.refresh_token,
        opts.auth_credentials_store_mode,
    )
}
