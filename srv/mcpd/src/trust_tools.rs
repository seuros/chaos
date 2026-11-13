//! Project trust administration for the operator: list, show, set, plan and
//! apply over `chaos serve --mcp`. These tools are registered only on the
//! authenticated HTTP endpoint, and only when serve has an operator token, never
//! on stdio; each call also requires the principal that token establishes. The
//! shared serve bearer cannot reach them, and an agent never holds the operator
//! token (serve strips it from child environments), so neither a trigger client
//! nor an agent can change which projects are trusted. The core lives in `chaos_kern::trust_admin`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chaos_kern::trust_admin::{self, Decision};
use mcp_host::prelude::*;
use mcp_host::registry::router::{McpToolRouter, tool_info_with_output};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::McpServerHandle;

/// The principal the `chaos serve --operator-token` credential authenticates as.
pub const OPERATOR_PRINCIPAL: &str = "chaos-operator";

pub(crate) struct TrustOperator {
    home: PathBuf,
}

/// Register the operator trust tools on an HTTP MCP server. Call this only for
/// a server behind the operator credential.
pub fn register_operator_trust_tools(
    handle: &McpServerHandle,
    chaos_home: &Path,
) -> std::io::Result<()> {
    tool_router()
        .register_all(
            handle.server.tool_registry(),
            Arc::new(TrustOperator {
                home: chaos_home.to_path_buf(),
            }),
        )
        .map_err(std::io::Error::other)
}

/// The recorded actor for a call, or a refusal unless the caller authenticated
/// as the operator.
fn operator_actor(ctx: &ExecutionContext<'_>) -> Result<String, ToolError> {
    match ctx.authenticated_principal() {
        Some(principal)
            if principal.source == mcp_host::HTTP_BEARER_PRINCIPAL_SOURCE
                && principal.subject == OPERATOR_PRINCIPAL =>
        {
            Ok(format!(
                "{OPERATOR_PRINCIPAL} (serve --mcp session {})",
                ctx.session_id()
            ))
        }
        _ => Err(ToolError::Execution(
            "project trust is an operator decision: this endpoint requires the operator token"
                .to_string(),
        )),
    }
}

fn structured(value: impl serde::Serialize) -> Result<ToolOutput, ToolError> {
    let value = serde_json::to_value(value).map_err(|err| ToolError::Execution(err.to_string()))?;
    ToolOutput::structured(value).map_err(|err| ToolError::Execution(err.to_string()))
}

fn failed(err: anyhow::Error) -> ToolError {
    ToolError::Execution(format!("{err:#}"))
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ToolError> {
    serde_json::from_value(params).map_err(|err| ToolError::InvalidArguments(err.to_string()))
}

type Fut<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>>;

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn any_object() -> Value {
    json!({"type": "object"})
}

// trust_list

fn list_info() -> ToolInfo {
    tool_info_with_output(
        "trust_list",
        None,
        Some(
            "List every stored project trust decision on this installation, granted and denied. \
             Changes nothing and never creates an installation identity."
                .to_string(),
        ),
        object(json!({}), &[]),
        any_object(),
    )
}

fn list_handler<'a>(server: &'a TrustOperator, ctx: ExecutionContext<'a>) -> Fut<'a> {
    Box::pin(crate::telemetry::tool("trust_list", async move {
        operator_actor(&ctx)?;
        structured(trust_admin::list(&server.home).await.map_err(failed)?)
    }))
}

// trust_show

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShowParams {
    project_path: String,
}

fn show_info() -> ToolInfo {
    tool_info_with_output(
        "trust_show",
        None,
        Some(
            "Show the key trust is decided under for a directory (its Git root, or itself) \
             and the decisions stored at that key and at the exact path. Changes nothing."
                .to_string(),
        ),
        object(
            json!({"project_path": {"type": "string", "description": "Absolute directory path."}}),
            &["project_path"],
        ),
        any_object(),
    )
}

fn show_handler<'a>(server: &'a TrustOperator, ctx: ExecutionContext<'a>) -> Fut<'a> {
    Box::pin(crate::telemetry::tool("trust_show", async move {
        operator_actor(&ctx)?;
        let params: ShowParams = parse(ctx.params.clone())?;
        let listing = trust_admin::list(&server.home).await.map_err(failed)?;
        let stored = |key: &str| {
            listing
                .entries
                .iter()
                .find(|entry| entry.project_path == key)
                .map_or(Decision::None, |entry| entry.decision)
        };
        let (exact, key) =
            match trust_admin::normalize_project_path(Path::new(&params.project_path)) {
                Ok(path) => {
                    let key = trust_admin::trust_key(&path).map_err(failed)?;
                    (
                        path.to_string_lossy().to_string(),
                        Some(key.to_string_lossy().to_string()),
                    )
                }
                // A path that no longer resolves has no root; show its exact key only.
                Err(_) => (params.project_path.clone(), None),
            };
        structured(json!({
            "installation_id": listing.installation_id,
            "storage": listing.storage,
            "project_path": exact,
            "stored_at_path": stored(&exact),
            "key": key,
            "stored_at_key": key.as_deref().map(stored),
        }))
    }))
}

// trust_set

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetParams {
    project_path: String,
    decision: Decision,
    expect: Decision,
}

fn set_info() -> ToolInfo {
    let decision = json!({"type": "string", "enum": ["trusted", "untrusted"]});
    tool_info_with_output(
        "trust_set",
        None,
        Some(
            "Grant (trusted) or revoke (untrusted) trust for one project root. `expect` is the \
             decision you saw (none, trusted or untrusted): if the stored decision differs when \
             prepared or at commit, nothing changes. Returns a receipt; the change is audited \
             in configuration_events with the operator as actor."
                .to_string(),
        ),
        object(
            json!({
                "project_path": {"type": "string", "description": "Absolute project root."},
                "decision": decision,
                "expect": {"type": "string", "enum": ["none", "trusted", "untrusted"]}
            }),
            &["project_path", "decision", "expect"],
        ),
        any_object(),
    )
}

fn set_handler<'a>(server: &'a TrustOperator, ctx: ExecutionContext<'a>) -> Fut<'a> {
    Box::pin(crate::telemetry::tool("trust_set", async move {
        let actor = operator_actor(&ctx)?;
        let params: SetParams = parse(ctx.params.clone())?;
        let prepared = trust_admin::prepare_set(
            &server.home,
            Path::new(&params.project_path),
            params.decision,
            params.expect,
        )
        .await
        .map_err(failed)?;
        structured(prepared.commit(&actor).await.map_err(failed)?)
    }))
}

// trust_export

fn export_info() -> ToolInfo {
    tool_info_with_output(
        "trust_export",
        None,
        Some(
            "Export every stored decision as a manifest bound to this installation, with its \
             digest. Review the manifest, then pass it and the digest to trust_plan and \
             trust_apply. Changes nothing."
                .to_string(),
        ),
        object(json!({}), &[]),
        any_object(),
    )
}

fn export_handler<'a>(server: &'a TrustOperator, ctx: ExecutionContext<'a>) -> Fut<'a> {
    Box::pin(crate::telemetry::tool("trust_export", async move {
        operator_actor(&ctx)?;
        let manifest = trust_admin::export(&server.home).await.map_err(failed)?;
        let digest = manifest.digest().map_err(failed)?;
        structured(json!({"manifest": manifest, "digest": digest}))
    }))
}

// trust_plan / trust_apply

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyParams {
    manifest: Value,
    digest: String,
    #[serde(default)]
    allow_held: bool,
}

fn manifest_schema(apply: bool) -> Value {
    let mut properties = json!({
        "manifest": {"type": "object", "description": "The manifest exactly as exported and reviewed."},
        "digest": {"type": "string", "description": "The digest you reviewed."}
    });
    if apply {
        properties["allow_held"] = json!({
            "type": "boolean",
            "description": "Commit even if some grants are held (a partial apply). Default false: any held grant refuses the apply."
        });
    }
    object(properties, &["manifest", "digest"])
}

fn plan_info() -> ToolInfo {
    tool_info_with_output(
        "trust_plan",
        None,
        Some(
            "Check a reviewed manifest against this installation without writing: the changes, \
             the unchanged entries, and any grants that would be held with the reason. Run it \
             before stopping a resident for a migration; a held grant means the cutover \
             cannot carry every decision."
                .to_string(),
        ),
        manifest_schema(false),
        any_object(),
    )
}

fn plan_handler<'a>(server: &'a TrustOperator, ctx: ExecutionContext<'a>) -> Fut<'a> {
    Box::pin(crate::telemetry::tool("trust_plan", async move {
        operator_actor(&ctx)?;
        let params: ApplyParams = parse(ctx.params.clone())?;
        let manifest = serde_json::to_string(&params.manifest)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        structured(
            trust_admin::prepare_apply(&server.home, &manifest, &params.digest)
                .await
                .map_err(failed)?,
        )
    }))
}

fn apply_info() -> ToolInfo {
    tool_info_with_output(
        "trust_apply",
        None,
        Some(
            "Apply a reviewed manifest to this installation. Refuses a manifest for another \
             installation, a digest that is not the reviewed one, and any entry whose stored \
             decision differs from what the manifest expects; every committed entry is checked \
             again at commit. Refuses if any grant would be held unless allow_held is true. \
             Returns a receipt; each change is audited with the operator as actor."
                .to_string(),
        ),
        manifest_schema(true),
        any_object(),
    )
}

fn apply_handler<'a>(server: &'a TrustOperator, ctx: ExecutionContext<'a>) -> Fut<'a> {
    Box::pin(crate::telemetry::tool("trust_apply", async move {
        let actor = operator_actor(&ctx)?;
        let params: ApplyParams = parse(ctx.params.clone())?;
        let manifest = serde_json::to_string(&params.manifest)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let prepared = trust_admin::prepare_apply(&server.home, &manifest, &params.digest)
            .await
            .map_err(failed)?;
        if !prepared.held.is_empty() && !params.allow_held {
            return Err(ToolError::Execution(format!(
                "refusing: {} grant(s) would be held and not carried; nothing was changed. \
                 Run trust_plan for the reasons, or pass allow_held for a partial apply:\n{}",
                prepared.held.len(),
                prepared
                    .held
                    .iter()
                    .map(|held| format!("{}: {}", held.project_path, held.reason))
                    .collect::<Vec<_>>()
                    .join("\n")
            )));
        }
        structured(prepared.commit(&actor).await.map_err(failed)?)
    }))
}

pub(crate) fn tool_router() -> McpToolRouter<TrustOperator> {
    McpToolRouter::new()
        .with_tool(list_info(), list_handler, None)
        .with_tool(show_info(), show_handler, None)
        .with_tool(set_info(), set_handler, None)
        .with_tool(export_info(), export_handler, None)
        .with_tool(plan_info(), plan_handler, None)
        .with_tool(apply_info(), apply_handler, None)
}

#[cfg(test)]
mod tests;
