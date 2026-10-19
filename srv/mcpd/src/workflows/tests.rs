use crate::{McpServerHandle, build_server, workflow_app};
use anyhow::{Context, Result};
use chaos_argv::Arg0DispatchPaths;
use chaos_kern::config::{ConfigBuilder, ConfigOverrides};
use mcp_host::prelude::Server;
use mcp_host::protocol::types::JsonRpcRequest;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

async fn factory(home: &tempfile::TempDir) -> Result<McpServerHandle> {
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "storage_url = \"sqlite://{}\"\n",
            home.path().join("chaos.sqlite").display()
        ),
    )?;
    let mut config = ConfigBuilder::default()
        .chaos_home(home.path().to_path_buf())
        .harness_overrides(ConfigOverrides {
            cwd: Some(home.path().to_path_buf()),
            ..Default::default()
        })
        .build()
        .await?;
    // Offline catalog/configuration only; none of these tests executes a leaf
    // that can invoke a model, shell, MCP child, or external service.
    config.clamp = true;
    Ok(build_server(
        Arc::new(config),
        Arg0DispatchPaths {
            alcatraz_exe: home.path().join("unused-alcatraz"),
        },
    )
    .await?)
}

async fn rpc(server: &Server, session: &str, method: &str, params: Value) -> Result<Value> {
    // Go through the actual SDK handler, not Tool::execute with a synthetic
    // context: root workflows require this originating request ID.
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        server.handle_request(
            session,
            JsonRpcRequest::new(json!("workflow-factory-test"), method, Some(params)),
        ),
    )
    .await?
    .context("SDK response")?;
    let response = serde_json::to_value(response)?;
    assert_eq!(response["id"], "workflow-factory-test");
    Ok(response)
}

async fn initialize(
    server: &Server,
    session: &str,
    version: &str,
    capabilities: Value,
) -> Result<Value> {
    let response = rpc(
        server,
        session,
        "initialize",
        json!({
            "protocolVersion":version,
            "capabilities":capabilities,
            "clientInfo":{"name":"workflow-factory-test","version":"1"}
        }),
    )
    .await?;
    assert!(response.get("error").is_none(), "{response}");
    server
        .handle_request(
            session,
            JsonRpcRequest::notification("notifications/initialized", None),
        )
        .await;
    assert!(
        server
            .get_session(session)
            .context("initialized session")?
            .is_initialized()
    );
    Ok(response["result"].clone())
}

async fn call(server: &Server, session: &str, name: &str, arguments: Value) -> Result<Value> {
    let response = rpc(
        server,
        session,
        "tools/call",
        json!({"name":name,"arguments":arguments}),
    )
    .await?;
    assert!(response.get("error").is_none(), "{response}");
    Ok(response["result"].clone())
}

fn payload(result: &Value) -> Result<Value> {
    anyhow::ensure!(result["isError"] != true, "{result}");
    if let Some(value) = result.get("structuredContent") {
        return Ok(value.clone());
    }
    Ok(serde_json::from_str(
        result["content"][0]["text"]
            .as_str()
            .context("JSON text fallback")?,
    )?)
}

fn condition() -> Value {
    json!({"tree":{
        "type":"condition","id":"check","pointer":"/input","predicate":{"op":"exists"}
    }})
}

async fn enable(server: &Server, session: &str) -> Result<()> {
    let result = call(
        server,
        session,
        "enable_tools",
        json!({"groups":["workflows"]}),
    )
    .await?;
    anyhow::ensure!(result["isError"] != true, "{result}");
    Ok(())
}

async fn tools(server: &Server, session: &str) -> Result<Vec<Value>> {
    let response = rpc(server, session, "tools/list", json!({})).await?;
    assert!(response.get("error").is_none(), "{response}");
    Ok(response["result"]["tools"]
        .as_array()
        .context("tool listing")?
        .clone())
}

#[tokio::test]
async fn factory_old_client_runs_without_tasks_and_cannot_read_or_cancel_another_owner()
-> Result<()> {
    let home = tempfile::tempdir()?;
    let mut handle = factory(&home).await?;
    let server = &handle.server;
    initialize(server, "owner", "2025-06-18", json!({})).await?;
    initialize(server, "foreign", "2025-06-18", json!({})).await?;
    let listing = tools(server, "owner").await?;
    let runner = listing
        .iter()
        .find(|tool| tool["name"] == "run_workflow")
        .context("old client still sees optional-task workflow")?;
    assert!(runner.get("execution").is_none(), "{runner}");

    let disabled = call(server, "owner", "run_workflow", condition()).await?;
    assert_eq!(
        disabled["isError"], true,
        "workflows default disabled: {disabled}"
    );
    enable(server, "owner").await?;
    let first = payload(&call(server, "owner", "run_workflow", condition()).await?)?;
    assert_eq!(first["kind"], "workflow");
    assert_eq!(first["status"]["outcome"], "success");
    let fallback = first["text_fallback"]
        .as_str()
        .context("old-client text guidance")?;
    assert!(fallback.contains(first["run_id"].as_str().context("run ID")?));
    assert!(fallback.contains("workflow_status"));
    assert!(fallback.contains("cancel_workflow"));
    let leaf_control = call(
        server,
        "owner",
        "run_workflow",
        json!({"tree":{
            "type":"call","id":"forbidden-control","tool":"disable_tools",
            "arguments":{"groups":["workflows"]}
        }}),
    )
    .await?;
    assert_eq!(leaf_control["isError"], true, "{leaf_control}");
    assert!(
        leaf_control.to_string().contains("control tools"),
        "{leaf_control}"
    );
    // The first operation process has already shut down. Enabling belongs to
    // its SDK owner, so a fresh dedicated process must keep the same setting.
    let second = payload(&call(server, "owner", "run_workflow", condition()).await?)?;
    assert_eq!(second["status"]["outcome"], "success");
    assert_ne!(first["run_id"], second["run_id"]);
    assert!(second.get("task").is_none());

    let foreign_disabled = call(server, "foreign", "run_workflow", condition()).await?;
    assert_eq!(foreign_disabled["isError"], true, "{foreign_disabled}");
    enable(server, "foreign").await?;
    for name in ["workflow_status", "cancel_workflow"] {
        let denied = call(server, "foreign", name, json!({"run_id":first["run_id"]})).await?;
        assert_eq!(denied["isError"], true, "{denied}");
        assert!(denied.to_string().contains("unknown run"), "{denied}");
    }
    let status = payload(
        &call(
            server,
            "owner",
            "workflow_status",
            json!({
                "run_id":first["run_id"]
            }),
        )
        .await?,
    )?;
    assert_eq!(status["run_id"], first["run_id"]);
    assert_eq!(status["status"]["outcome"], "success");

    let disabled = call(
        server,
        "owner",
        "disable_tools",
        json!({"groups":["workflows"]}),
    )
    .await?;
    assert_ne!(disabled["isError"], true, "{disabled}");
    let refused = call(server, "owner", "run_workflow", condition()).await?;
    assert_eq!(refused["isError"], true, "{refused}");
    // Disabling admission cannot erase retained owned status.
    let retained = payload(
        &call(
            server,
            "owner",
            "workflow_status",
            json!({
                "run_id":first["run_id"]
            }),
        )
        .await?,
    )?;
    assert_eq!(retained["run_id"], first["run_id"]);
    let foreign_run = payload(&call(server, "foreign", "run_workflow", condition()).await?)?;
    assert_eq!(foreign_run["status"]["outcome"], "success");
    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn factory_optional_tasks_preserve_origin_and_enforce_sdk_task_ownership() -> Result<()> {
    let home = tempfile::tempdir()?;
    let mut handle = factory(&home).await?;
    let server = &handle.server;
    let initialized = initialize(server, "task-owner", "2025-11-25", json!({})).await?;
    initialize(server, "task-foreign", "2025-11-25", json!({})).await?;
    assert!(initialized["capabilities"]["tasks"]["requests"]["tools"]["call"].is_object());
    for name in ["run_workflow", "run_synopsis"] {
        let listing = tools(server, "task-owner").await?;
        let tool = listing
            .iter()
            .find(|tool| tool["name"] == name)
            .context("root endpoint")?;
        assert_eq!(tool["execution"]["taskSupport"], "optional");
    }
    enable(server, "task-owner").await?;
    let started = rpc(
        server,
        "task-owner",
        "tools/call",
        json!({
            "name":"run_workflow", "arguments":condition(), "task":{"ttl":60000}
        }),
    )
    .await?;
    assert!(started.get("error").is_none(), "{started}");
    let task_id = started["result"]["task"]["taskId"]
        .as_str()
        .context("SDK task ID")?;
    for method in ["tasks/get", "tasks/result", "tasks/cancel"] {
        let denied = rpc(server, "task-foreign", method, json!({"taskId":task_id})).await?;
        assert!(denied.get("error").is_some(), "{denied}");
    }
    let completed = rpc(
        server,
        "task-owner",
        "tasks/result",
        json!({"taskId":task_id}),
    )
    .await?;
    assert!(completed.get("error").is_none(), "{completed}");
    let snapshot = payload(&completed["result"])?;
    assert_eq!(snapshot["status"]["outcome"], "success");
    let status = payload(
        &call(
            server,
            "task-owner",
            "workflow_status",
            json!({
                "run_id":snapshot["run_id"]
            }),
        )
        .await?,
    )?;
    assert_eq!(status["run_id"], snapshot["run_id"]);
    // Optional means the same current-protocol client can still call normally.
    let synchronous = payload(&call(server, "task-owner", "run_workflow", condition()).await?)?;
    assert_eq!(synchronous["status"]["outcome"], "success");
    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn factory_apps_negotiation_gates_dashboard_and_exposes_static_view_metadata() -> Result<()> {
    let home = tempfile::tempdir()?;
    let mut handle = factory(&home).await?;
    let server = &handle.server;
    initialize(server, "text-only", "2025-11-25", json!({})).await?;
    let apps = initialize(
        server,
        "apps",
        "2025-11-25",
        json!({
            "extensions":{"io.modelcontextprotocol/ui":{"mimeTypes":["text/html;profile=mcp-app"]}}
        }),
    )
    .await?;
    assert_eq!(
        apps["capabilities"]["extensions"]["io.modelcontextprotocol/ui"]["mimeTypes"],
        json!(["text/html;profile=mcp-app"])
    );
    assert!(
        !tools(server, "text-only")
            .await?
            .iter()
            .any(|tool| tool["name"] == workflow_app::DASHBOARD_TOOL)
    );
    let refused = rpc(
        server,
        "text-only",
        "tools/call",
        json!({
            "name":workflow_app::DASHBOARD_TOOL,"arguments":{}
        }),
    )
    .await?;
    assert!(
        refused.get("error").is_some() || refused["result"]["isError"] == true,
        "{refused}"
    );
    let hidden = rpc(server, "text-only", "resources/list", json!({})).await?;
    assert!(
        !hidden["result"]["resources"]
            .as_array()
            .context("resources")?
            .iter()
            .any(|resource| resource["uri"] == workflow_app::DASHBOARD_URI)
    );
    let denied = rpc(
        server,
        "text-only",
        "resources/read",
        json!({
            "uri":workflow_app::DASHBOARD_URI
        }),
    )
    .await?;
    assert!(denied.get("error").is_some(), "{denied}");

    let listing = tools(server, "apps").await?;
    let dashboard = listing
        .iter()
        .find(|tool| tool["name"] == workflow_app::DASHBOARD_TOOL)
        .context("Apps dashboard endpoint")?;
    assert_eq!(
        dashboard["_meta"]["ui"]["resourceUri"],
        workflow_app::DASHBOARD_URI
    );
    assert_eq!(dashboard["_meta"]["ui"]["visibility"], json!(["app"]));
    let runner = listing
        .iter()
        .find(|tool| tool["name"] == "run_workflow")
        .context("workflow")?;
    assert_eq!(
        runner["_meta"]["ui"]["resourceUri"],
        workflow_app::DASHBOARD_URI
    );
    enable(server, "apps").await?;
    let snapshot = payload(&call(server, "apps", "run_workflow", condition()).await?)?;
    let summary = payload(&call(server, "apps", workflow_app::DASHBOARD_TOOL, json!({})).await?)?;
    assert_eq!(summary["details_omitted"], true);
    let runs = summary["runs"].as_array().context("dashboard runs")?;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["run_id"], snapshot["run_id"]);
    assert!(runs[0].get("result").is_none());
    for step in runs[0]["steps"].as_array().context("summary steps")? {
        assert!(step.get("detail").is_none());
    }
    let resource = rpc(
        server,
        "apps",
        "resources/read",
        json!({
            "uri":workflow_app::DASHBOARD_URI
        }),
    )
    .await?;
    assert!(resource.get("error").is_none(), "{resource}");
    let content = &resource["result"]["contents"][0];
    assert_eq!(content["mimeType"], "text/html;profile=mcp-app");
    assert_eq!(content["text"], workflow_app::DASHBOARD_HTML);
    assert_eq!(content["_meta"]["ui"]["csp"]["connectDomains"], json!([]));
    assert_eq!(content["_meta"]["ui"]["permissions"], json!({}));
    assert!(
        !content["text"]
            .as_str()
            .context("HTML")?
            .contains(snapshot["run_id"].as_str().context("run ID")?)
    );
    handle.shutdown().await?;
    Ok(())
}
