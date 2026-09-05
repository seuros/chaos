use chaos_ipc::ProcessId;
use chaos_ipc::openai_models::ModelPreset;
use chaos_ipc::protocol::McpAuthStatus;
use chaos_ipc::protocol::McpStartupStatus;
use chaos_sysctl::types::McpServerConfig;
use chaos_sysctl::types::McpServerTransportConfig;

use crate::models_manager::manager::ProviderModels;
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;

use crate::runtime_db::RuntimeDbHandle;

mod man;

pub const JSON_MIME_TYPE: &str = "application/json";
pub const CHAOS_SESSIONS_URI: &str = "chaos://sessions";
pub const CHAOS_SESSIONS_URI_TEMPLATE: &str = "chaos://sessions/{id}";
pub const CHAOS_CRONS_URI: &str = "chaos://crons";
pub const CHAOS_SPOOL_URI: &str = "chaos://spool";
pub const CHAOS_MODELS_URI: &str = "chaos://models";
pub const CHAOS_MODES_URI: &str = "chaos://modes";
pub const CHAOS_MCP_URI: &str = "chaos://mcp";
pub use man::MANUAL_INDEX_URI as CHAOS_MANUAL_URI;
pub use man::MANUAL_PAGE_URI_TEMPLATE as CHAOS_MANUAL_URI_TEMPLATE;
pub use man::MARKDOWN_MIME_TYPE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChaosBuiltinResourceKind {
    Sessions,
    Crons,
    Spool,
    Models,
    Modes,
    Mcp,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChaosBuiltinResourceTemplateKind {
    SessionDetail,
    ManualPage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChaosBuiltinResourceSpec {
    pub kind: ChaosBuiltinResourceKind,
    pub uri: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub mime_type: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChaosBuiltinResourceTemplateSpec {
    pub kind: ChaosBuiltinResourceTemplateKind,
    pub uri_template: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub mime_type: &'static str,
}

const RESOURCE_SPECS: [ChaosBuiltinResourceSpec; 7] = [
    ChaosBuiltinResourceSpec {
        kind: ChaosBuiltinResourceKind::Sessions,
        uri: CHAOS_SESSIONS_URI,
        name: "sessions",
        description: "List all ChaOS processes",
        mime_type: JSON_MIME_TYPE,
    },
    ChaosBuiltinResourceSpec {
        kind: ChaosBuiltinResourceKind::Crons,
        uri: CHAOS_CRONS_URI,
        name: "crons",
        description: "List all scheduled cron jobs",
        mime_type: JSON_MIME_TYPE,
    },
    ChaosBuiltinResourceSpec {
        kind: ChaosBuiltinResourceKind::Spool,
        uri: CHAOS_SPOOL_URI,
        name: "spool",
        description: "List all persisted spool jobs",
        mime_type: JSON_MIME_TYPE,
    },
    ChaosBuiltinResourceSpec {
        kind: ChaosBuiltinResourceKind::Models,
        uri: CHAOS_MODELS_URI,
        name: "models",
        description: "List every model preset available to this ChaOS installation",
        mime_type: JSON_MIME_TYPE,
    },
    ChaosBuiltinResourceSpec {
        kind: ChaosBuiltinResourceKind::Modes,
        uri: CHAOS_MODES_URI,
        name: "modes",
        description: "List the ChaOS collaboration modes visible to this caller",
        mime_type: JSON_MIME_TYPE,
    },
    ChaosBuiltinResourceSpec {
        kind: ChaosBuiltinResourceKind::Mcp,
        uri: CHAOS_MCP_URI,
        name: "mcp",
        description: "List configured MCP servers with authentication and startup status",
        mime_type: JSON_MIME_TYPE,
    },
    ChaosBuiltinResourceSpec {
        kind: ChaosBuiltinResourceKind::Manual,
        uri: CHAOS_MANUAL_URI,
        name: "manual",
        description: "List agent-facing ChaOS manual pages and their resource URIs",
        mime_type: JSON_MIME_TYPE,
    },
];

const RESOURCE_TEMPLATE_SPECS: [ChaosBuiltinResourceTemplateSpec; 2] = [
    ChaosBuiltinResourceTemplateSpec {
        kind: ChaosBuiltinResourceTemplateKind::SessionDetail,
        uri_template: CHAOS_SESSIONS_URI_TEMPLATE,
        name: "session_detail",
        description: "Details for a specific ChaOS process",
        mime_type: JSON_MIME_TYPE,
    },
    ChaosBuiltinResourceTemplateSpec {
        kind: ChaosBuiltinResourceTemplateKind::ManualPage,
        uri_template: CHAOS_MANUAL_URI_TEMPLATE,
        name: "manual_page",
        description: "Read an agent-facing ChaOS manual page by canonical page id",
        mime_type: MARKDOWN_MIME_TYPE,
    },
];

pub fn resource_specs() -> &'static [ChaosBuiltinResourceSpec] {
    &RESOURCE_SPECS
}

pub fn resource_template_specs() -> &'static [ChaosBuiltinResourceTemplateSpec] {
    &RESOURCE_TEMPLATE_SPECS
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedChaosBuiltinResource {
    Sessions,
    SessionDetail { process_id: ProcessId },
    Crons,
    Spool,
    Models,
    Modes,
    Mcp,
    ManualIndex,
    ManualPage(&'static man::ManualPageSpec),
}

pub fn resolve_resource_uri(uri: &str) -> Result<Option<ResolvedChaosBuiltinResource>, String> {
    if let Some(id) = uri.strip_prefix("chaos://sessions/") {
        if id.is_empty() {
            return Err("missing process_id in resource URI".to_string());
        }
        let process_id = ProcessId::from_string(id)
            .map_err(|err| format!("invalid process_id in resource URI: {err}"))?;
        return Ok(Some(ResolvedChaosBuiltinResource::SessionDetail {
            process_id,
        }));
    }

    match uri {
        CHAOS_SESSIONS_URI => Ok(Some(ResolvedChaosBuiltinResource::Sessions)),
        CHAOS_CRONS_URI => Ok(Some(ResolvedChaosBuiltinResource::Crons)),
        CHAOS_SPOOL_URI => Ok(Some(ResolvedChaosBuiltinResource::Spool)),
        CHAOS_MODELS_URI => Ok(Some(ResolvedChaosBuiltinResource::Models)),
        CHAOS_MODES_URI => Ok(Some(ResolvedChaosBuiltinResource::Modes)),
        CHAOS_MCP_URI => Ok(Some(ResolvedChaosBuiltinResource::Mcp)),
        _ => match man::resolve_resource_uri(uri)? {
            Some(man::ResolvedManualResource::Index) => {
                Ok(Some(ResolvedChaosBuiltinResource::ManualIndex))
            }
            Some(man::ResolvedManualResource::Page(page)) => {
                Ok(Some(ResolvedChaosBuiltinResource::ManualPage(page)))
            }
            None => Ok(None),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChaosBuiltinResourceContent {
    pub text: String,
    pub mime_type: &'static str,
}

fn json_resource(text: String) -> ChaosBuiltinResourceContent {
    ChaosBuiltinResourceContent {
        text,
        mime_type: JSON_MIME_TYPE,
    }
}

// Resource text is model-facing: avoid spending context on JSON indentation.
fn to_json<T: Serialize>(value: &T, context: &str) -> Result<String, String> {
    serde_json::to_string(value)
        .map_err(|err| format!("failed to serialize {context} resource: {err}"))
}

pub async fn sessions_json_from_runtime_db(
    runtime_db: Option<&RuntimeDbHandle>,
) -> Result<String, String> {
    let sessions = match runtime_db {
        Some(runtime) => {
            let page = runtime
                .list_processes(
                    50,
                    None,
                    chaos_proc::SortKey::UpdatedAt,
                    &[],
                    None,
                    false,
                    None,
                )
                .await
                .map_err(|err| format!("failed to list ChaOS processes: {err}"))?;
            page.items
                .iter()
                .map(|process| {
                    json!({
                        "process_id": process.id.to_string(),
                        "title": process.title,
                        "source": process.source,
                        "cwd": process.cwd,
                        "updated_at": process.updated_at.to_string(),
                        "tokens_used": process.tokens_used,
                    })
                })
                .collect::<Vec<_>>()
        }
        None => Vec::new(),
    };

    to_json(&sessions, "ChaOS processes")
}

pub async fn session_detail_json_from_runtime_db(
    runtime_db: Option<&RuntimeDbHandle>,
    process_id: ProcessId,
) -> Result<String, String> {
    let runtime = runtime_db.ok_or_else(|| {
        "ChaOS session resources require a persisted runtime database".to_string()
    })?;
    let process = runtime
        .get_process(process_id)
        .await
        .map_err(|err| format!("failed to read ChaOS process {process_id}: {err}"))?
        .ok_or_else(|| format!("process not found: {process_id}"))?;

    to_json(
        &json!({
            "process_id": process.id.to_string(),
            "title": process.title,
            "source": process.source,
            "cwd": process.cwd,
            "created_at": process.created_at.to_string(),
            "updated_at": process.updated_at.to_string(),
            "model_provider": process.model_provider,
            "sandbox_policy": process.sandbox_policy,
            "approval_mode": process.approval_mode,
            "tokens_used": process.tokens_used,
            "first_user_message": process.first_user_message,
            "git_branch": process.git_branch,
        }),
        "ChaOS process",
    )
}

/// Just enough to choose with: what to ask for, what it is good at, and which
/// reasoning efforts it will accept. Picker flags, modalities and transport
/// details stay behind.
fn model_json(preset: &ModelPreset) -> serde_json::Value {
    let mut model = serde_json::Map::new();
    model.insert("id".to_string(), json!(preset.model));
    // Providers reached through a bare `/models` listing publish neither of
    // these. Emitting them empty spends tokens to say nothing.
    if !preset.description.is_empty() {
        model.insert("description".to_string(), json!(preset.description));
    }
    if !preset.supported_reasoning_efforts.is_empty() {
        model.insert(
            "reasoning_efforts".to_string(),
            json!(
                preset
                    .supported_reasoning_efforts
                    .iter()
                    .map(|effort| effort.effort)
                    .collect::<Vec<_>>()
            ),
        );
    }
    serde_json::Value::Object(model)
}

pub fn models_json_from_provider_models(groups: &[ProviderModels]) -> Result<String, String> {
    let providers = groups
        .iter()
        .map(|group| {
            json!({
                "provider": group.provider_id,
                "active": group.active,
                "models": group.models.iter().map(model_json).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();

    to_json(&providers, "ChaOS models")
}

pub fn modes_json_from_chaos_home(chaos_home: &Path) -> Result<String, String> {
    let registry = crate::modes::ModeRegistry::load(
        chaos_home,
        crate::collaboration_modes::CollaborationModesConfig::default(),
    )
    .map_err(|err| format!("failed to load ChaOS mode catalog: {err}"))?;
    let policy = crate::modes::ModePolicy::root(&registry);
    registry.installation_resource_json(&policy)
}

pub async fn mcp_json_from_config(
    revision: Option<u64>,
    config: &crate::config::Config,
    active_servers: Option<&HashMap<String, McpServerConfig>>,
    startup_statuses: Option<&HashMap<String, McpStartupStatus>>,
) -> Result<String, String> {
    let servers = config.mcp_servers.get();
    let auth_statuses = crate::mcp::auth::compute_auth_statuses(
        servers.iter(),
        config.mcp_oauth_credentials_store_mode,
    )
    .await
    .into_iter()
    .map(|(name, entry)| (name, entry.auth_status))
    .collect();
    mcp_json_from_servers(
        revision,
        servers,
        active_servers,
        &auth_statuses,
        startup_statuses,
    )
}

pub fn mcp_json_from_servers(
    revision: Option<u64>,
    servers: &HashMap<String, McpServerConfig>,
    active_servers: Option<&HashMap<String, McpServerConfig>>,
    auth_statuses: &HashMap<String, McpAuthStatus>,
    startup_statuses: Option<&HashMap<String, McpStartupStatus>>,
) -> Result<String, String> {
    let mut names = servers.keys().collect::<Vec<_>>();
    names.sort();
    let servers = names
        .into_iter()
        .map(|name| {
            let config = &servers[name];
            let transport = match &config.transport {
                McpServerTransportConfig::Stdio { .. } => "stdio",
                McpServerTransportConfig::StreamableHttp { .. } => "streamable_http",
            };
            let status = if !config.enabled {
                json!({ "state": "disabled" })
            } else {
                match (active_servers, startup_statuses) {
                    (Some(active_servers), Some(_))
                        if active_servers.get(name) != Some(config) =>
                    {
                        json!({
                            "state": "not_loaded",
                            "error": "configured server is not active in this session; the latest reload may have failed"
                        })
                    }
                    (Some(_), Some(startup_statuses)) => startup_statuses
                        .get(name)
                        .map(|status| match status {
                            McpStartupStatus::Starting => json!({ "state": "starting" }),
                            McpStartupStatus::Ready => json!({ "state": "ready" }),
                            McpStartupStatus::Failed { error } => {
                                json!({ "state": "failed", "error": error })
                            }
                            McpStartupStatus::Cancelled => json!({ "state": "cancelled" }),
                        })
                        .unwrap_or_else(|| {
                            json!({
                                "state": "not_loaded",
                                "error": "configured server has no active client"
                            })
                        }),
                    _ => json!({
                        "state": "unavailable",
                        "error": "per-session status requires an active ChaOS session"
                    }),
                }
            };

            let mut server = serde_json::Map::new();
            server.insert("name".to_string(), json!(name));
            server.insert("enabled".to_string(), json!(config.enabled));
            server.insert("required".to_string(), json!(config.required));
            server.insert("transport".to_string(), json!(transport));
            server.insert(
                "auth_status".to_string(),
                auth_statuses.get(name).map_or(serde_json::Value::Null, |status| json!(status)),
            );
            server.insert("status".to_string(), status);
            if let Some(reason) = &config.disabled_reason {
                server.insert("disabled_reason".to_string(), json!(reason.to_string()));
            }
            serde_json::Value::Object(server)
        })
        .collect::<Vec<_>>();

    to_json(
        &json!({
            "revision": revision,
            "servers": servers,
        }),
        "MCP server status",
    )
}

pub async fn crons_json() -> Result<String, String> {
    chaos_cron::resource::list_crons().await
}

pub async fn spool_json() -> Result<String, String> {
    chaos_cron::resource::list_spool().await
}

#[allow(async_fn_in_trait)]
pub trait ChaosBuiltinResourceBackend {
    async fn sessions_json(&self) -> Result<String, String>;
    async fn session_detail_json(&self, process_id: ProcessId) -> Result<String, String>;
    async fn crons_json(&self) -> Result<String, String>;
    async fn spool_json(&self) -> Result<String, String>;
    async fn models_json(&self) -> Result<String, String>;
    async fn modes_json(&self) -> Result<String, String>;
    async fn mcp_json(&self) -> Result<String, String>;
}

pub async fn read_resource<B: ChaosBuiltinResourceBackend + Sync>(
    backend: &B,
    uri: &str,
) -> Result<Option<ChaosBuiltinResourceContent>, String> {
    match resolve_resource_uri(uri)? {
        Some(ResolvedChaosBuiltinResource::Sessions) => {
            backend.sessions_json().await.map(json_resource).map(Some)
        }
        Some(ResolvedChaosBuiltinResource::SessionDetail { process_id }) => backend
            .session_detail_json(process_id)
            .await
            .map(json_resource)
            .map(Some),
        Some(ResolvedChaosBuiltinResource::Crons) => {
            backend.crons_json().await.map(json_resource).map(Some)
        }
        Some(ResolvedChaosBuiltinResource::Spool) => {
            backend.spool_json().await.map(json_resource).map(Some)
        }
        Some(ResolvedChaosBuiltinResource::Models) => {
            backend.models_json().await.map(json_resource).map(Some)
        }
        Some(ResolvedChaosBuiltinResource::Modes) => {
            backend.modes_json().await.map(json_resource).map(Some)
        }
        Some(ResolvedChaosBuiltinResource::Mcp) => {
            backend.mcp_json().await.map(json_resource).map(Some)
        }
        Some(ResolvedChaosBuiltinResource::ManualIndex) => {
            man::index_json().map(json_resource).map(Some)
        }
        Some(ResolvedChaosBuiltinResource::ManualPage(page)) => {
            Ok(Some(ChaosBuiltinResourceContent {
                text: man::render_page(page),
                mime_type: MARKDOWN_MIME_TYPE,
            }))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_builtin_resource_uris() {
        assert_eq!(
            resolve_resource_uri(CHAOS_SESSIONS_URI).expect("resolve sessions"),
            Some(ResolvedChaosBuiltinResource::Sessions)
        );
        assert_eq!(
            resolve_resource_uri(CHAOS_CRONS_URI).expect("resolve crons"),
            Some(ResolvedChaosBuiltinResource::Crons)
        );
        assert_eq!(
            resolve_resource_uri(CHAOS_SPOOL_URI).expect("resolve spool"),
            Some(ResolvedChaosBuiltinResource::Spool)
        );
        assert_eq!(
            resolve_resource_uri(CHAOS_MODELS_URI).expect("resolve models"),
            Some(ResolvedChaosBuiltinResource::Models)
        );
        assert_eq!(
            resolve_resource_uri(CHAOS_MODES_URI).expect("resolve modes"),
            Some(ResolvedChaosBuiltinResource::Modes)
        );
        assert_eq!(
            resolve_resource_uri(CHAOS_MCP_URI).expect("resolve mcp"),
            Some(ResolvedChaosBuiltinResource::Mcp)
        );
        assert_eq!(
            resolve_resource_uri(CHAOS_MANUAL_URI).expect("resolve manual"),
            Some(ResolvedChaosBuiltinResource::ManualIndex)
        );
    }

    #[test]
    fn resolves_session_detail_uri() {
        let process_id = ProcessId::default();
        let uri = format!("chaos://sessions/{process_id}");

        assert_eq!(
            resolve_resource_uri(&uri).expect("resolve session detail"),
            Some(ResolvedChaosBuiltinResource::SessionDetail { process_id })
        );
    }

    #[test]
    fn rejects_invalid_session_detail_uri() {
        let err = resolve_resource_uri("chaos://sessions/not-a-uuid").expect_err("invalid uri");
        assert!(err.contains("invalid process_id"));
    }

    #[test]
    fn resolves_manual_page_uri() {
        assert!(matches!(
            resolve_resource_uri("chaos://man/chaos-mcp.7").expect("resolve manual page"),
            Some(ResolvedChaosBuiltinResource::ManualPage(page)) if page.id == "chaos-mcp.7"
        ));
    }

    #[test]
    fn mcp_status_json_is_sorted_and_does_not_serialize_secrets() {
        let alpha: McpServerConfig = serde_json::from_value(json!({
            "url": "https://example.com/mcp",
            "bearer_token": "secret"
        }))
        .expect("HTTP config");
        let beta: McpServerConfig = serde_json::from_value(json!({
            "command": "server",
            "env": { "API_TOKEN": "secret" },
            "enabled": false
        }))
        .expect("stdio config");
        let gamma: McpServerConfig = serde_json::from_value(json!({
            "command": "broken-server"
        }))
        .expect("failed stdio config");
        let servers = HashMap::from([
            ("beta".to_string(), beta),
            ("alpha".to_string(), alpha.clone()),
            ("gamma".to_string(), gamma.clone()),
        ]);
        let active = HashMap::from([("alpha".to_string(), alpha), ("gamma".to_string(), gamma)]);
        let auth = HashMap::from([("alpha".to_string(), McpAuthStatus::BearerToken)]);
        let startup_statuses = HashMap::from([
            ("alpha".to_string(), McpStartupStatus::Ready),
            (
                "gamma".to_string(),
                McpStartupStatus::Failed {
                    error: "401 Unauthorized".to_string(),
                },
            ),
        ]);

        let text = mcp_json_from_servers(
            Some(3),
            &servers,
            Some(&active),
            &auth,
            Some(&startup_statuses),
        )
        .expect("MCP status JSON");
        assert!(!text.contains('\n'), "model-facing JSON must be compact");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse status JSON");

        assert_eq!(value["revision"], 3);
        assert_eq!(value["servers"][0]["name"], "alpha");
        assert_eq!(value["servers"][0]["auth_status"], "bearer_token");
        assert_eq!(value["servers"][0]["status"]["state"], "ready");
        assert_eq!(value["servers"][1]["name"], "beta");
        assert_eq!(value["servers"][1]["status"]["state"], "disabled");
        assert_eq!(value["servers"][2]["name"], "gamma");
        assert_eq!(value["servers"][2]["status"]["state"], "failed");
        assert_eq!(value["servers"][2]["status"]["error"], "401 Unauthorized");
        assert!(!text.contains("secret"));
        assert!(!text.contains("API_TOKEN"));
    }

    #[test]
    fn mcp_status_is_unavailable_without_an_active_session() {
        let config: McpServerConfig = serde_json::from_value(json!({
            "command": "server"
        }))
        .expect("stdio config");
        let servers = HashMap::from([("server".to_string(), config)]);

        let text = mcp_json_from_servers(None, &servers, None, &HashMap::new(), None)
            .expect("MCP status JSON");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse status JSON");

        assert_eq!(value["servers"][0]["status"]["state"], "unavailable");
    }
}
