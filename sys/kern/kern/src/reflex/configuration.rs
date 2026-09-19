//! Reflex settings and credential storage.

use anyhow::{Context, ensure};
use chaos_sysctl::edit::{ConfigEdit, ConfigEditsBuilder};
use chaos_sysctl::secrets;
use url::Url;

use crate::AuthManager;
use crate::config::{Config, ReflexBackendSettings, ReflexKind};
use crate::env::read_non_empty_env_var;

/// Interactive setup presets.
pub fn presets() -> [(&'static str, ReflexBackendSettings); 4] {
    let settings = |kind, base_url: &str| ReflexBackendSettings {
        kind,
        base_url: Some(base_url.into()),
        model: None,
        path: None,
        api_key: None,
        auth_provider: None,
        env_key: None,
        timeout_ms: None,
        allow_remote_fallback: false,
    };
    let mut openrouter = settings(ReflexKind::Jev, "https://openrouter.ai/api");
    openrouter.path = Some("/alpha/decisions".into());
    openrouter.model = Some("typesafe/jev-1.13".into());
    [
        (
            "typesafe",
            settings(ReflexKind::Jev, chaos_reflex::jev::DEFAULT_BASE_URL),
        ),
        ("openrouter", openrouter),
        (
            "minicheck",
            settings(ReflexKind::Minicheck, "http://localhost:11434/v1"),
        ),
        (
            "shieldgemma",
            settings(ReflexKind::Shieldgemma, "http://localhost:11434/v1"),
        ),
    ]
}

fn endpoint(settings: &ReflexBackendSettings) -> anyhow::Result<Url> {
    let base = settings.base_url.as_deref().or_else(|| {
        (settings.kind == ReflexKind::Jev).then_some(chaos_reflex::jev::DEFAULT_BASE_URL)
    });
    let url = Url::parse(base.context("this backend requires a base URL")?)
        .map_err(|_| anyhow::anyhow!("invalid reflex base URL"))?;
    ensure!(
        matches!(url.scheme(), "https" | "http") && url.host_str().is_some(),
        "reflex endpoints must use HTTP or HTTPS"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "put credentials in the API key field, not in the endpoint URL"
    );
    Ok(url)
}

fn require_secure_transport(endpoint: &Url) -> anyhow::Result<()> {
    let loopback = match endpoint.host() {
        Some(url::Host::Domain(host)) => host == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    ensure!(
        endpoint.scheme() == "https" || loopback,
        "credential-bearing endpoints require HTTPS (except loopback)"
    );
    Ok(())
}

/// Validate without reading any credentials or contacting the endpoint.
pub fn validate(config: &Config, settings: &ReflexBackendSettings) -> anyhow::Result<()> {
    let endpoint = endpoint(settings)?;
    let sources = [
        settings.api_key.as_deref(),
        settings.auth_provider.as_deref(),
        settings.env_key.as_deref(),
    ];
    ensure!(
        sources.iter().flatten().count() <= 1,
        "choose one credential source: saved key, provider account, or environment"
    );
    ensure!(
        sources
            .iter()
            .flatten()
            .all(|value| !value.trim().is_empty()),
        "credential sources must not be blank"
    );
    if let Some(reference) = &settings.api_key {
        ensure!(
            secrets::is_reference(reference),
            "literal API keys are not valid settings"
        );
        secrets::externalize(reference)?;
    }
    if sources.iter().any(Option::is_some) {
        require_secure_transport(&endpoint)?;
    }
    if let Some(provider_id) = &settings.auth_provider {
        let provider = config
            .model_providers
            .get(provider_id)
            .context("unknown provider account; connect it using /accounts")?;
        let provider_url = provider
            .base_url
            .as_deref()
            .and_then(|base| Url::parse(base).ok())
            .context("provider account must have an explicit base URL")?;
        ensure!(
            endpoint.origin() == provider_url.origin(),
            "provider account and reflex endpoint must have the same origin"
        );
    }
    if let Some(path) = &settings.path {
        ensure!(
            settings.kind == ReflexKind::Jev
                && path.starts_with('/')
                && !path.starts_with("//")
                && !path
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, '?' | '#' | '\\')),
            "decisions path must be an absolute URL path and is only supported by Jev"
        );
    }
    ensure!(
        !settings.model().trim().is_empty(),
        "model must not be blank"
    );
    ensure!(
        settings.timeout_ms != Some(0),
        "timeout must be greater than zero"
    );
    Ok(())
}

pub(super) fn resolve_api_key(
    config: &Config,
    settings: &ReflexBackendSettings,
    auth: Option<&AuthManager>,
) -> anyhow::Result<Option<String>> {
    validate(config, settings)?;
    let key =
        if let Some(reference) = &settings.api_key {
            Some(secrets::resolve(reference).map_err(|_| {
                anyhow::anyhow!("saved reflex credential is unavailable; open /reflex")
            })?)
        } else if let Some(provider) = &settings.auth_provider {
            Some(
                auth.and_then(|auth| auth.auth_for_provider(provider))
                    .and_then(|auth| auth.api_key().map(str::to_owned))
                    .context("provider has no saved API key; connect it using /accounts")?,
            )
        } else if let Some(env_key) = &settings.env_key {
            Some(
                read_non_empty_env_var(env_key)
                    .context("configured reflex environment key is unset")?,
            )
        } else {
            None
        };
    ensure!(
        key.as_ref().is_none_or(|key| !key.trim().is_empty()),
        "reflex credential is empty"
    );
    ensure!(
        settings.kind != ReflexKind::Jev || key.is_some(),
        "Jev requires a saved key or provider account; open /reflex"
    );
    Ok(key)
}

/// Save settings atomically; store new keys in the OS keyring.
pub async fn save_backend(
    config: &Config,
    auth: &AuthManager,
    name: &str,
    mut settings: ReflexBackendSettings,
    new_key: Option<&str>,
) -> anyhow::Result<ReflexBackendSettings> {
    ensure!(
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
        "backend name may contain only letters, digits, hyphens, and underscores"
    );
    let new_key = new_key.map(str::trim).filter(|key| !key.is_empty());
    if let Some(key) = new_key {
        ensure!(
            !secrets::is_reference(key)
                && !key.chars().any(|c| c.is_whitespace() || c.is_control()),
            "enter an API key, not a reference or multi-line value"
        );
        ensure!(
            settings.auth_provider.is_none(),
            "choose a provider account or enter a new key, not both"
        );
        settings.api_key = None;
        settings.env_key = None;
        require_secure_transport(&endpoint(&settings)?)?;
    } else if let Some(reference) = &settings.api_key {
        let origin = endpoint(&settings)?.origin();
        for previous in config
            .reflex
            .values()
            .filter(|previous| previous.api_key.as_ref() == Some(reference))
        {
            ensure!(
                endpoint(previous)?.origin() == origin,
                "enter a new key when changing the credential's endpoint origin"
            );
        }
    }
    validate(config, &settings)?;
    let new_reference = new_key.map(secrets::externalize).transpose()?;
    if let Some(reference) = &new_reference {
        settings.api_key = Some(reference.clone());
    }
    let result = async {
        resolve_api_key(config, &settings, Some(auth))?;
        let document = toml::to_string(&settings)?.parse::<toml_edit::DocumentMut>()?;
        crate::user_settings::install_persistence();
        ConfigEditsBuilder::new(&config.chaos_home)
            .with_edits([ConfigEdit::SetPath {
                segments: vec!["reflex".into(), name.into()],
                value: document.as_item().clone(),
            }])
            .apply()
            .await
    }
    .await;
    if result.is_err()
        && let Some(reference) = new_reference
        && secrets::remove(&reference).is_err()
    {
        tracing::warn!("could not clean up an unused reflex credential");
    }
    result?;
    Ok(settings)
}
