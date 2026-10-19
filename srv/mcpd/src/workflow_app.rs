//! Static MCP Apps view; session ownership belongs to the tool handlers, not HTML.
//!
//! Integration: opt in with `ServerBuilder::with_mcp_apps()`, mount
//! `resource_router()`, link model-visible workflow tools with `linked_tool_ui()`,
//! and mark the owner-scoped `workflow_dashboard` tool with `dashboard_tool_ui()`.
//! Return `{ "runs": [RunSnapshot, ...], "next_offset": usize?,
//! "details_omitted": true }` as structured content (at most ten runs/page).
//! The SDK supplies JSON text for structured output; `dashboard_text()` is also
//! available for text-only surfaces. Never substitute run data into HTML.

use mcp_host::prelude::*;
use mcp_host::protocol::apps;
use mcp_host::registry::router::McpResourceRouter;
use serde_json::Value;

pub(crate) const DASHBOARD_URI: &str = "ui://chaos/workflows";
pub(crate) const DASHBOARD_TOOL: &str = "workflow_dashboard";
pub(crate) const CANCEL_TOOL: &str = "cancel_workflow";
pub(crate) const DASHBOARD_HTML: &str = include_str!("workflow_app/dashboard.html");
pub(crate) const TEXT_FALLBACK: &str = "Workflow dashboard: use workflow_status with a run_id \
    to inspect a session-owned run, or cancel_workflow with run_id to request cancellation. \
    Cancellation is not completion until cleanup finishes. Pending input must be answered \
    through the host's existing interaction flow, not this dashboard.";

/// Link a workflow tool without changing its model/app visibility.
pub(crate) fn linked_tool_ui() -> Result<ToolUi, AppsError> {
    ToolUi::new(DASHBOARD_URI)
}

/// The SDK hides/refuses app-only tools for clients without negotiated Apps.
///
/// This metadata is not an ownership check. The handler must use the caller's
/// authoritative session registry for both listing and cancellation.
pub(crate) fn dashboard_tool_ui() -> Result<ToolUi, AppsError> {
    Ok(linked_tool_ui()?.with_visibility([UiVisibility::App]))
}

/// No remote origins and no requested browser permissions.
pub(crate) fn resource_policy() -> UiResourceMeta {
    UiResourceMeta {
        csp: Some(UiCsp {
            connect_domains: Some(vec![]),
            resource_domains: Some(vec![]),
            frame_domains: Some(vec![]),
            base_uri_domains: Some(vec![]),
        }),
        permissions: Some(UiPermissions::default()),
        domain: None,
        prefers_border: Some(true),
    }
}

fn apps_supported(ctx: &VisibilityContext<'_>) -> bool {
    ctx.client_capabilities()
        .is_some_and(ClientCapabilities::supports_mcp_apps)
}

/// Static resource: does not read a registry, request params, or session data.
pub(crate) fn resource_content() -> Result<ResourceContent, AppsError> {
    Ok(ResourceContent::mcp_app(DASHBOARD_URI, DASHBOARD_HTML)
        .with_meta(resource_policy().into_meta()?))
}

fn read_dashboard<'a, S>(_: &'a S, ctx: ExecutionContext<'a>) -> ResourceReadFuture<'a> {
    Box::pin(async move {
        if !apps_supported(&ctx) {
            return Err(ResourceError::NotFound(TEXT_FALLBACK.into()));
        }
        Ok(vec![resource_content()?])
    })
}

/// Mount using the same service Arc as the other resource routers.
pub(crate) fn resource_router<S>() -> Result<McpResourceRouter<S>, AppsError> {
    let info = ResourceInfo {
        uri: DASHBOARD_URI.into(),
        name: "workflow_dashboard".into(),
        title: Some("Session workflows".into()),
        description: Some(TEXT_FALLBACK.into()),
        mime_type: Some(apps::MIME_TYPE.into()),
        icons: None,
        annotations: None,
        size: Some(DASHBOARD_HTML.len() as u64),
        meta: Some(resource_policy().into_meta()?),
        subscribable: false,
    };
    Ok(McpResourceRouter::new().with_resource(info, read_dashboard::<S>, Some(apps_supported)))
}

/// Bounded, fixed-schema text alternative for already-authorized snapshots.
///
/// Accepts the dashboard envelope or one `RunSnapshot`. Deliberately excludes
/// arbitrary result/detail data and does not interpret it as instructions.
pub(crate) fn dashboard_text(payload: &Value) -> String {
    let runs = payload
        .get("runs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_else(|| {
            if payload.get("run_id").is_some() {
                std::slice::from_ref(payload)
            } else {
                &[]
            }
        });
    let mut text = String::from("Session workflows\n");
    if runs.is_empty() {
        text.push_str("No runs on this page.\n");
    }
    for run in runs.iter().take(16) {
        text.push_str(&format!(
            "Run {} ({}) — {}{}\n",
            label(&run["run_id"]),
            label(&run["kind"]),
            status_text(&run["status"]),
            if run["truncated"] == true {
                " [truncated]"
            } else {
                ""
            },
        ));
        if let Some(steps) = run["steps"].as_array() {
            for step in steps.iter().take(32) {
                text.push_str(&format!(
                    "  {} attempt {} — {}{}\n",
                    label(&step["node_id"]),
                    step["attempt"].as_u64().unwrap_or(0),
                    status_text(&step["status"]),
                    if step["truncated"] == true {
                        " [truncated]"
                    } else {
                        ""
                    },
                ));
            }
            if steps.len() > 32 {
                text.push_str("  [display truncated: more attempts omitted]\n");
            }
        }
    }
    if runs.len() > 16 {
        text.push_str("[display truncated: more runs omitted]\n");
    }
    if let Some(offset) = payload["next_offset"].as_u64() {
        text.push_str(&format!(
            "More runs: request workflow_dashboard with offset {offset} in the app.\n"
        ));
    }
    if payload["details_omitted"] == true {
        text.push_str("Details/root payloads intentionally omitted from this summary.\n");
    }
    // Bounds total output even when every allowed label reaches its limit.
    let clipped = text.chars().count() > 15_000;
    let mut text: String = text.chars().take(15_000).collect();
    if clipped {
        text.push_str("\n[display truncated]\n");
    }
    text.push_str(TEXT_FALLBACK);
    text
}

fn label(value: &Value) -> String {
    value
        .as_str()
        .unwrap_or("unknown")
        .chars()
        .filter(|character| !character.is_control())
        .take(96)
        .collect()
}

fn status_text(status: &Value) -> String {
    match status["state"].as_str() {
        Some("terminal") => format!("terminal: {}", label(&status["outcome"])),
        Some("waiting_for_input") => "pending input (use host interaction flow)".into(),
        Some("cancelling") => "cancelling (cleanup pending)".into(),
        Some("running") => "running".into(),
        _ => "unknown status".into(),
    }
}

#[cfg(test)]
mod tests;
