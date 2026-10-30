//! Private policy material is hashed in memory, never persisted or returned.
use std::collections::BTreeMap;
use std::io::Read;

use chaos_kern::{
    AuthManager,
    config::{AgentRoleSource, Config},
};
use serde_json::{Value, json};

use super::*;

pub(crate) struct PinnedPolicy {
    config: Arc<Config>,
    auth: Arc<AuthManager>,
    digest: String,
    execution_config: Arc<Config>,
}

impl PinnedPolicy {
    pub(crate) fn new(config: Arc<Config>, auth: Arc<AuthManager>) -> Result<Self, ApiError> {
        let providers: Vec<_> = config.model_providers.keys().cloned().collect();
        let auth = auth.pinned_for_providers(&config.model_provider_id, &providers);
        let execution_config = Arc::new(freeze_roles(&config)?);
        let digest = fingerprint(&execution_config, &auth)?;
        Ok(Self {
            config,
            auth,
            digest,
            execution_config,
        })
    }

    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
    pub(crate) fn auth(&self) -> Arc<AuthManager> {
        self.auth.clone()
    }
    pub(crate) fn config(&self) -> Arc<Config> {
        self.execution_config.clone()
    }

    pub(crate) fn check(&self) -> Result<(), ApiError> {
        if fingerprint(&freeze_roles(&self.config)?, &self.auth)? != self.digest {
            return Err(ApiError::conflict(
                "session credentials or execution policy changed",
            ));
        }
        Ok(())
    }
}

fn freeze_roles(config: &Config) -> Result<Config, ApiError> {
    let mut snapshot = config.clone();
    for role in snapshot.agent_roles.values_mut() {
        if let Some(AgentRoleSource::File(path)) = &role.source {
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .map_err(|_| unavailable())?
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| unavailable())?;
            if bytes.len() > 1024 * 1024 {
                return Err(unavailable());
            }
            let text = std::str::from_utf8(&bytes).map_err(|_| unavailable())?;
            let config = toml::from_str(text).map_err(|_| unavailable())?;
            role.source = Some(AgentRoleSource::Database {
                config,
                base_dir: path.parent().map(std::path::Path::to_path_buf),
            });
        }
    }
    Ok(snapshot)
}

fn fingerprint(config: &Config, auth: &AuthManager) -> Result<String, ApiError> {
    config
        .model_provider
        .api_key()
        .map_err(|_| ApiError::conflict("configured provider credential source is unavailable"))?;
    let credential = auth.auth_cached();
    if config.model_provider.requires_openai_auth
        && credential.is_none()
        && config.model_provider.env_key.is_none()
        && config.model_provider.experimental_bearer_token.is_none()
    {
        return Err(ApiError::conflict(
            "durable sessions require provider credentials",
        ));
    }
    let servers = serde_json::to_value(config.mcp_servers.get()).map_err(|_| unavailable())?;
    for server in servers.as_object().ok_or_else(unavailable)?.values() {
        if server.get("enabled") == Some(&json!(false)) || server.get("url").is_none() {
            continue;
        }
        // Implicit MCP OAuth currently has no pinned subject available here.
        // Fail closed instead of claiming its mutable credential store is isolated.
        let nonempty = |value: &str| !value.trim().is_empty();
        let env_present = |name: &str| std::env::var(name).ok().is_some_and(|v| nonempty(&v));
        let explicit = server
            .get("bearer_token")
            .and_then(Value::as_str)
            .is_some_and(nonempty)
            || server
                .get("bearer_token_env_var")
                .and_then(Value::as_str)
                .is_some_and(env_present)
            || ["http_headers", "env_http_headers"].iter().any(|field| {
                server
                    .get(*field)
                    .and_then(Value::as_object)
                    .is_some_and(|headers| {
                        headers.iter().any(|(name, value)| {
                            name.eq_ignore_ascii_case("authorization")
                                && value.as_str().is_some_and(|v| {
                                    if *field == "env_http_headers" {
                                        env_present(v)
                                    } else {
                                        nonempty(v)
                                    }
                                })
                        })
                    })
            });
        if !explicit {
            return Err(ApiError::conflict(
                "durable HTTP MCP connections require explicit credential sources",
            ));
        }
    }
    let p = &config.permissions;
    let env = &p.shell_environment_policy;
    let clamp_settings = config.clamp_settings();
    let roles: BTreeMap<_, _> = config
        .agent_roles
        .iter()
        .map(|(name, role)| {
            let source = match &role.source {
                Some(AgentRoleSource::BuiltIn(path)) => json!({"builtin": path}),
                Some(AgentRoleSource::File(_)) => return Err(unavailable()),
                Some(AgentRoleSource::Database { config, base_dir }) => {
                    json!({"config": config, "base_dir": base_dir})
                }
                None => Value::Null,
            };
            Ok((
                name,
                json!({
                    "source": source, "description": role.description,
                    "nicknames": role.nickname_candidates, "topics": role.topics,
                    "catchphrases": role.catchphrases,
                }),
            ))
        })
        .collect::<Result<_, ApiError>>()?;
    let mut material = json!({
        "format": "httpd-policy/v1",
        "configured": config.config_layer_stack.effective_config(),
        "provider_id": config.model_provider_id,
        "provider": config.model_provider,
        "providers": config.model_providers,
        "egress": config.egress_url,
        "cwd": config.cwd,
        "home": config.chaos_home,
        "permissions": {
            "approval": p.approval_policy.get(), "sandbox": p.sandbox_policy.get(),
            "vfs": p.vfs_policy, "sockets": p.socket_policy,
            "network": p.network.as_ref().map(|network| network.policy_snapshot()),
            "login_shell": p.allow_login_shell,
            "environment": {
                "inherit": env.inherit, "ignore_excludes": env.ignore_default_excludes,
                "exclude": env.exclude.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "include": env.include_only.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "set": env.r#set, "profile": env.use_profile,
            },
            "macos": p.macos_seatbelt_profile_extensions,
        },
        "tools": servers,
        "reflex": config.reflex,
        "roles": roles,
        "instructions": {
            "base": config.base_instructions, "developer": config.developer_instructions,
            "user": config.user_instructions, "minion": config.minion_instructions,
        },
        "execution": {
            "model": config.model, "reasoning": config.model_reasoning_effort,
            "reviewer": config.approvals_reviewer, "trust": config.active_project_trust,
            "collab": config.collab_enabled, "minions": config.minion_jobs_allowed,
            "max_depth": config.agent_max_depth, "max_threads": config.agent_max_threads,
            "hook_policy": config.hook_approval_policy,
            "web_search": config.web_search_mode.get(),
            "sandbox_helper": config.alcatraz_exe,
        },
        "clamp_transport": {
            "enabled": config.clamp,
            "backend": format!("{:?}", clamp_settings.backend),
            "environment_exclusions": clamp_settings.env_remove,
            "claude_resume_dir": clamp_settings.claude_resume_dir,
            "claude_cwd": clamp_settings.claude_cwd,
            "antigravity": clamp_settings.antigravity,
        },
        "credential": credential_material(credential.as_ref())?,
        "provider_credentials": config.model_providers.keys().map(|id| {
            let credential = auth.auth_for_provider(id);
            Ok((id.clone(), credential_material(credential.as_ref())?))
        }).collect::<Result<BTreeMap<_, _>, ApiError>>()?,
    });
    let mut environment = BTreeMap::new();
    environment_sources(&material, &mut environment);
    material["environment_sources"] = json!(environment);
    let digest = chaos_kern::user_settings::fingerprint(&material).map_err(|_| unavailable())?;
    Ok(digest
        .strip_prefix("v1:")
        .ok_or_else(unavailable)?
        .to_owned())
}

fn credential_material(credential: Option<&chaos_kern::ChaosAuth>) -> Result<Value, ApiError> {
    let Some(credential) = credential else {
        return Ok(Value::Null);
    };
    let subject = credential
        .credential_subject_fingerprint("httpd-policy/v1")
        .map(|subject| subject.into_string());
    let account = credential.get_account_id();
    if !credential.is_api_key_auth() && subject.is_none() && account.is_none() {
        return Err(ApiError::conflict(
            "durable sessions require a stable credential identity",
        ));
    }
    Ok(json!({
        "mode": format!("{:?}", credential.api_auth_mode()),
        "subject": subject, "account": account, "user": credential.get_chatgpt_user_id(),
        // Bind API keys and stable OAuth identity, not rotating OAuth tokens.
        "api_key": credential.api_key(),
    }))
}

fn environment_sources(value: &Value, resolved: &mut BTreeMap<String, Option<String>>) {
    let mut add = |name: &str| {
        resolved.insert(name.to_owned(), std::env::var(name).ok());
    };
    if let Some(object) = value.as_object() {
        for (name, value) in object {
            match name.as_str() {
                "env_key" | "bearer_token_env_var" => {
                    if let Some(name) = value.as_str() {
                        add(name);
                    }
                }
                "env_vars" => {
                    if let Some(names) = value.as_array() {
                        for name in names.iter().filter_map(Value::as_str) {
                            add(name);
                        }
                    }
                }
                "env_http_headers" => {
                    if let Some(headers) = value.as_object() {
                        for name in headers.values().filter_map(Value::as_str) {
                            add(name);
                        }
                    }
                }
                _ => {}
            }
        }
        for child in object.values() {
            environment_sources(child, resolved);
        }
    } else if let Some(array) = value.as_array() {
        for child in array {
            environment_sources(child, resolved);
        }
    }
}
