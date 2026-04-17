use chaos_ipc::ProcessId;
use chaos_storage::ChaosStorageProvider;
use serde::Serialize;
use serde_json::json;

use crate::runtime_db::RuntimeDbHandle;

pub const JSON_MIME_TYPE: &str = "application/json";
pub const CHAOS_SESSIONS_URI: &str = "chaos://sessions";
pub const CHAOS_SESSIONS_URI_TEMPLATE: &str = "chaos://sessions/{id}";
pub const CHAOS_CRONS_URI: &str = "chaos://crons";
pub const CHAOS_SPOOL_URI: &str = "chaos://spool";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChaosBuiltinResourceKind {
    Sessions,
    Crons,
    Spool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChaosBuiltinResourceTemplateKind {
    SessionDetail,
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

const RESOURCE_SPECS: [ChaosBuiltinResourceSpec; 3] = [
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
];

const RESOURCE_TEMPLATE_SPECS: [ChaosBuiltinResourceTemplateSpec; 1] =
    [ChaosBuiltinResourceTemplateSpec {
        kind: ChaosBuiltinResourceTemplateKind::SessionDetail,
        uri_template: CHAOS_SESSIONS_URI_TEMPLATE,
        name: "session_detail",
        description: "Details for a specific ChaOS process",
        mime_type: JSON_MIME_TYPE,
    }];

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
}

pub fn resolve_resource_uri(uri: &str) -> Result<Option<ResolvedChaosBuiltinResource>, String> {
    match uri {
        CHAOS_SESSIONS_URI => Ok(Some(ResolvedChaosBuiltinResource::Sessions)),
        CHAOS_CRONS_URI => Ok(Some(ResolvedChaosBuiltinResource::Crons)),
        CHAOS_SPOOL_URI => Ok(Some(ResolvedChaosBuiltinResource::Spool)),
        _ => {
            let Some(id) = uri.strip_prefix("chaos://sessions/") else {
                return Ok(None);
            };
            if id.is_empty() {
                return Err("missing process_id in resource URI".to_string());
            }
            let process_id = ProcessId::from_string(id)
                .map_err(|err| format!("invalid process_id in resource URI: {err}"))?;
            Ok(Some(ResolvedChaosBuiltinResource::SessionDetail {
                process_id,
            }))
        }
    }
}

fn to_pretty_json<T: Serialize>(value: &T, context: &str) -> Result<String, String> {
    serde_json::to_string_pretty(value)
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

    to_pretty_json(&sessions, "ChaOS processes")
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

    to_pretty_json(
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

pub async fn crons_json_from_provider(
    provider: Option<&ChaosStorageProvider>,
) -> Result<String, String> {
    chaos_cron::resource::list_crons(provider).await
}

pub async fn spool_json_from_provider(
    provider: Option<&ChaosStorageProvider>,
) -> Result<String, String> {
    chaos_cron::resource::list_spool(provider).await
}

#[allow(async_fn_in_trait)]
pub trait ChaosBuiltinResourceBackend {
    async fn sessions_json(&self) -> Result<String, String>;
    async fn session_detail_json(&self, process_id: ProcessId) -> Result<String, String>;
    async fn crons_json(&self) -> Result<String, String>;
    async fn spool_json(&self) -> Result<String, String>;
}

pub async fn read_resource_json<B: ChaosBuiltinResourceBackend + Sync>(
    backend: &B,
    uri: &str,
) -> Result<Option<String>, String> {
    match resolve_resource_uri(uri)? {
        Some(ResolvedChaosBuiltinResource::Sessions) => backend.sessions_json().await.map(Some),
        Some(ResolvedChaosBuiltinResource::SessionDetail { process_id }) => {
            backend.session_detail_json(process_id).await.map(Some)
        }
        Some(ResolvedChaosBuiltinResource::Crons) => backend.crons_json().await.map(Some),
        Some(ResolvedChaosBuiltinResource::Spool) => backend.spool_json().await.map(Some),
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
}
