use std::time::Duration;

use anyhow::{Context, Result};
use chaos_cron::CreateJobParams;
use chaos_cron::CronScope;
use chaos_cron::CronStore;
use chaos_proc::open_runtime_db;
use mcp_host::protocol::types::ErrorCode;
use mcp_host::protocol::types::JsonRpcMessage;
use mcp_host::protocol::types::RequestId;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

use mcp_test_support::McpProcess;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(20);

async fn spawn_mcp_process() -> Result<(TempDir, McpProcess)> {
    let chaos_home = TempDir::new()?;
    let mcp = McpProcess::new(chaos_home.path()).await?;
    Ok((chaos_home, mcp))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn negotiated_protocol_keeps_resources_templates_and_capability_gates() -> Result<()> {
    for (requested, negotiated) in [
        ("2025-06-18", "2025-06-18"),
        ("2025-11-25", "2025-11-25"),
        ("2099-01-01", "2025-11-25"),
    ] {
        let (_home, mut mcp) = spawn_mcp_process().await?;
        timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.initialize_expecting_version(requested, negotiated),
        )
        .await??;

        for (method, list_key, uri_key, other_key, required_uris) in [
            (
                "resources/list",
                "resources",
                "uri",
                "uriTemplate",
                &[
                    "chaos://sessions",
                    "chaos://crons",
                    "chaos://spool",
                    "chaos://models",
                    "chaos://modes",
                    "chaos://mcp",
                    "chaos://man",
                    "chaos://machine",
                ][..],
            ),
            (
                "resources/templates/list",
                "resourceTemplates",
                "uriTemplate",
                "uri",
                &["chaos://sessions/{id}", "chaos://man/{page}"][..],
            ),
        ] {
            let id = mcp.send_custom_request(method, None).await?;
            let response = timeout(
                DEFAULT_READ_TIMEOUT,
                mcp.read_stream_until_response_message(id.clone()),
            )
            .await??;
            assert_eq!(response.id, Some(id.to_value()), "{requested} {method}");
            let result = response
                .result
                .with_context(|| format!("{method} result"))?;
            let entries = result[list_key]
                .as_array()
                .with_context(|| format!("{requested} {list_key} array"))?;
            for entry in entries {
                let uri = entry[uri_key]
                    .as_str()
                    .with_context(|| format!("{requested} {uri_key} in {entry}"))?;
                assert_eq!(uri.contains('{'), uri_key == "uriTemplate", "{uri}");
                assert!(entry.get(other_key).is_none(), "{requested} {entry}");
            }
            for &uri in required_uris {
                assert!(
                    entries.iter().any(|entry| entry[uri_key] == uri),
                    "{requested} {method} missing {uri}"
                );
            }
        }
        // No negotiated session may call a capability the server did not declare.
        let id = mcp.send_custom_request("prompts/list", None).await?;
        let response = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_response_or_error_message(id),
        )
        .await??;
        let JsonRpcMessage::Response(response) = response else {
            anyhow::bail!("expected missing-capability response");
        };
        let error = response
            .error
            .ok_or_else(|| anyhow::anyhow!("missing capability accepted"))?;
        assert_eq!(error.code, ErrorCode::METHOD_NOT_FOUND, "{requested}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_list_before_initialize_is_rejected() -> Result<()> {
    let (_chaos_home, mut mcp) = spawn_mcp_process().await?;

    let request_id = mcp.send_custom_request("tools/list", None).await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(resp) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };
    let error = resp.error.as_ref().expect("expected error response");

    assert_eq!(resp.id, Some(request_id.to_value()));
    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(
        error.data,
        Some(json!({
            "code": "not_initialized",
            "type": "validation"
        }))
    );
    assert_eq!(
        error.message,
        "Session must complete notifications/initialized first"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_list_succeeds_after_initialize_response() -> Result<()> {
    let (_chaos_home, mut mcp) = spawn_mcp_process().await?;

    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp.send_custom_request("tools/list", None).await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(resp) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };

    assert_eq!(resp.id, Some(request_id.to_value()));
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
    let tools = resp.result.as_ref().unwrap()["tools"]
        .as_array()
        .expect("tools array");
    assert!(
        !tools.is_empty(),
        "tools/list should succeed immediately after initialize"
    );
    let refresh = tools
        .iter()
        .find(|tool| tool["name"] == "refresh_models")
        .expect("refresh_models advertised");
    assert_eq!(refresh["inputSchema"]["required"], json!(["provider"]));
    // Trust administration is operator-only: stdio never advertises it, so an
    // agent wired to `chaos mcp` cannot reach it.
    assert!(
        tools.iter().all(|tool| !tool["name"]
            .as_str()
            .unwrap_or_default()
            .starts_with("trust_")),
        "stdio must not list trust tools: {tools:?}"
    );

    Ok(())
}

async fn read_json_resource(mcp: &mut McpProcess, uri: &str) -> Result<String> {
    let request_id = mcp
        .send_custom_request("resources/read", Some(json!({ "uri": uri })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(request_id),
    )
    .await??;
    let result = response.result.with_context(|| format!("{uri} result"))?;
    let [content] = result["contents"]
        .as_array()
        .context("contents array")?
        .as_slice()
    else {
        anyhow::bail!("expected one resource for {uri}");
    };
    assert_eq!(content["uri"], uri);
    assert_eq!(content["mimeType"], "application/json");
    Ok(content["text"]
        .as_str()
        .context("resource text")?
        .to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_and_models_resources_can_be_read_after_initialize() -> Result<()> {
    let (_chaos_home, mut mcp) = spawn_mcp_process().await?;
    mcp.initialize().await?;

    let text = read_json_resource(&mut mcp, "chaos://mcp").await?;
    let payload: serde_json::Value = serde_json::from_str(&text)?;
    assert!(payload["revision"].is_null());
    assert!(payload["servers"].is_array());

    // Cold, default home: the catalog wire contract must not require a provider server.
    read_active_model_group(&mut mcp, &[]).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn machine_resource_can_be_read_after_initialize() -> Result<()> {
    let (_chaos_home, mut mcp) = spawn_mcp_process().await?;
    mcp.initialize().await?;
    let request_id = mcp
        .send_custom_request("resources/read", Some(json!({ "uri": "chaos://machine" })))
        .await?;
    let message = timeout(
        Duration::from_secs(40),
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;
    let JsonRpcMessage::Response(response) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };
    assert_eq!(response.id, Some(request_id.to_value()));
    assert!(
        response.error.is_none(),
        "unexpected error: {:?}",
        response.error
    );
    let content = &response.result.as_ref().unwrap()["contents"][0];
    assert_eq!(content["uri"], "chaos://machine");
    assert_eq!(content["mimeType"], "application/json");
    let text = content["text"].as_str().expect("machine observation text");
    assert!(!text.contains('\n'), "resource JSON stays compact");
    let payload: serde_json::Value = serde_json::from_str(text)?;
    assert_eq!(payload["scope"], "harness_host");
    assert!(payload["warnings"].is_array());
    assert!(payload["machine"]["observed_at"].is_object());
    assert!(payload["machine"]["profile"].is_object());
    assert!(payload["machine"]["power"].is_object());
    assert!(payload["machine"]["thermal"]["state"].is_string());
    assert!(payload["machine"]["thermal"]["cpu_temperatures"].is_array());
    assert!(payload["storage"]["filesystems"].is_array());
    assert!(payload["storage"]["unavailable"].is_array());
    assert!(payload["indexing"]["workspace"].is_string());
    assert!(payload["indexing"]["spotlight"]["status"].is_string());
    assert!(payload["indexing"]["processes"]["status"].is_string());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_resources_can_be_read_after_initialize() -> Result<()> {
    let (_chaos_home, mut mcp) = spawn_mcp_process().await?;
    mcp.initialize().await?;

    let index_request_id = mcp
        .send_custom_request("resources/read", Some(json!({ "uri": "chaos://man" })))
        .await?;
    let index_message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(index_request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(index_response) = index_message else {
        anyhow::bail!("expected JSON-RPC response, got: {index_message:?}");
    };
    assert_eq!(index_response.id, Some(index_request_id.to_value()));
    assert!(
        index_response.error.is_none(),
        "unexpected error: {:?}",
        index_response.error
    );

    let index_content = &index_response.result.as_ref().unwrap()["contents"][0];
    assert_eq!(index_content["uri"], json!("chaos://man"));
    assert_eq!(index_content["mimeType"], json!("application/json"));
    let index: serde_json::Value =
        serde_json::from_str(index_content["text"].as_str().expect("manual index text"))?;
    let pages = index["pages"].as_array().expect("manual pages array");
    assert!(
        pages
            .iter()
            .any(|page| page["uri"] == json!("chaos://man/chaos-appearance.7"))
    );
    assert!(
        pages
            .iter()
            .any(|page| page["uri"] == json!("chaos://man/chaos-httpd.8"))
    );
    assert!(!pages.iter().any(|page| page["id"] == json!("README")));
    assert!(
        pages
            .iter()
            .any(|page| page["uri"] == json!("chaos://man/chaos-mcp.7"))
    );

    for (id, heading, expected_body) in [
        ("chaos-mcp.7", "## NAME", "## SEE ALSO"),
        (
            "chaos-appearance.7",
            "## Configuration",
            "chaos config set appearance.user.bold true",
        ),
        ("chaos-httpd.8", "## NAME", "## SEE ALSO"),
    ] {
        let uri = format!("chaos://man/{id}");
        let page_request_id = mcp
            .send_custom_request("resources/read", Some(json!({ "uri": uri })))
            .await?;
        let page_message = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_response_or_error_message(page_request_id.clone()),
        )
        .await??;

        let JsonRpcMessage::Response(page_response) = page_message else {
            anyhow::bail!("expected JSON-RPC response, got: {page_message:?}");
        };
        assert_eq!(page_response.id, Some(page_request_id.to_value()));
        assert!(
            page_response.error.is_none(),
            "unexpected error: {:?}",
            page_response.error
        );

        let page_content = &page_response.result.as_ref().unwrap()["contents"][0];
        assert_eq!(page_content["uri"], json!(uri));
        assert_eq!(page_content["mimeType"], json!("text/markdown"));
        let page = page_content["text"].as_str().expect("manual page text");
        assert!(page.starts_with(heading));
        assert!(page.contains(expected_body));
        let (_, footer) = page
            .rsplit_once("\n\n---\n\n")
            .expect("manual navigation footer");
        assert_eq!(footer, "Index: `chaos://man`\n");
        assert!(!page.contains("+++"));
        assert!(!page.contains("](./"));
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_cron_and_spool_resources_preserve_their_exact_wire_envelopes() -> Result<()> {
    let (_chaos_home, mut mcp) = spawn_mcp_process().await?;
    mcp.initialize().await?;

    for uri in ["chaos://crons", "chaos://spool"] {
        let request_id = mcp
            .send_custom_request("resources/read", Some(json!({ "uri": uri })))
            .await?;
        let message = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_response_or_error_message(request_id.clone()),
        )
        .await??;
        let JsonRpcMessage::Response(resp) = message else {
            anyhow::bail!("expected JSON-RPC response for {uri}, got: {message:?}");
        };
        assert_eq!(resp.id, Some(request_id.to_value()), "{uri}");
        assert!(resp.error.is_none(), "{uri}: {:?}", resp.error);
        assert_eq!(
            resp.result.as_ref().unwrap(),
            &json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": "[]"
                }]
            }),
            "{uri}"
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_models_fetches_provider_catalog_through_mcp() -> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/models"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer fixture-key",
        ))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{
                "id": "fixture-model", "object": "model", "created": 0, "owned_by": "fixture"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let home = TempDir::new()?;
    // Exercise the real CLI import; only the key's env var name belongs in settings.
    std::fs::write(
        home.path().join(mcp_test_support::SETTINGS_FILE),
        serde_json::to_vec_pretty(&json!({
            "model_provider": "fixture",
            "model_providers": {
                "fixture": {
                    "name": "Fixture",
                    "base_url": server.uri(),
                    "wire_api": "responses",
                    "env_key": "FIXTURE_API_KEY",
                }
            }
        }))?,
    )?;
    let mut mcp =
        McpProcess::new_with_env(home.path(), &[("FIXTURE_API_KEY", Some("fixture-key"))]).await?;
    mcp.initialize().await?;
    let endpoint = server.uri();
    let private = ["FIXTURE_API_KEY", "fixture-key", endpoint.as_str()];
    let cold = read_active_model_group(&mut mcp, &private).await?;
    assert_eq!(cold["provider"], "fixture");
    assert_eq!(cold["models"], json!([]));
    assert!(
        server
            .received_requests()
            .await
            .context("recorded requests")?
            .is_empty(),
        "reading the cold catalog must not fetch models"
    );

    let request_id = mcp
        .send_custom_request(
            "tools/call",
            Some(json!({"name": "refresh_models", "arguments": {"provider": "fixture"}})),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(request_id),
    )
    .await??;
    let result = response.result.context("tool result")?;
    assert_ne!(result["isError"], json!(true), "{result}");
    let refreshed = &result["structuredContent"];
    assert_eq!(refreshed["provider"], "fixture");
    assert_eq!(refreshed["models"][0]["id"], "fixture-model");

    let warm = read_active_model_group(&mut mcp, &private).await?;
    assert_eq!(warm["provider"], "fixture");
    assert_eq!(warm["models"], refreshed["models"]);
    assert_eq!(
        server
            .received_requests()
            .await
            .context("recorded requests")?
            .len(),
        1,
        "only the explicit refresh should contact the provider"
    );
    Ok(())
}

async fn read_active_model_group(
    mcp: &mut McpProcess,
    private_values: &[&str],
) -> Result<serde_json::Value> {
    let text = read_json_resource(mcp, "chaos://models").await?;
    for forbidden in [
        "api_key",
        "bearer",
        "token",
        "Authorization",
        "base_url",
        "wire_api",
    ]
    .into_iter()
    .chain(private_values.iter().copied())
    {
        assert!(
            !text.contains(forbidden),
            "model catalog leaked '{forbidden}'"
        );
    }
    let groups: Vec<serde_json::Value> = serde_json::from_str(&text)?;
    for group in &groups {
        assert!(group["provider"].is_string(), "{group}");
        assert!(group["active"].is_boolean(), "{group}");
        assert!(group["models"].is_array(), "{group}");
    }
    let mut active = groups.into_iter().filter(|group| group["active"] == true);
    let group = active.next().context("missing active provider")?;
    assert!(active.next().is_none(), "multiple active providers");
    Ok(group)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cron_resource_reads_only_jobs_owned_by_this_mcp_session() -> Result<()> {
    let chaos_home = TempDir::new()?;
    let server = mcp_test_support::create_mock_responses_server(vec![
        mcp_test_support::create_final_assistant_message_sse_response("Owner ready")?,
    ])
    .await;
    super::chaos_tool::seed_mock_provider(chaos_home.path(), &server.uri()).await?;
    let pool = open_runtime_db(chaos_home.path()).await?;
    let store = CronStore::new(pool);
    store
        .create(&CreateJobParams::shell(
            "persisted job".to_string(),
            chaos_cron::Schedule::Interval { seconds: 300 }.to_json(),
            "echo hi".to_string(),
            CronScope::Project,
            Some("/tmp/project".to_string()),
            None,
        ))
        .await?;
    store
        .create(&CreateJobParams::shell(
            "foreign session job".into(),
            chaos_cron::Schedule::Interval { seconds: 300 }.to_json(),
            "echo foreign-private-command".into(),
            CronScope::Session,
            None,
            Some("00000000-0000-7000-8000-000000000001".into()),
        ))
        .await?;

    let mut mcp = McpProcess::new(chaos_home.path()).await?;
    mcp.initialize().await?;

    let call_id = mcp
        .send_chaos_tool_call(chaos_mcpd::ChaosToolParams {
            prompt: "say hi".into(),
            ..Default::default()
        })
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Number(call_id.into())),
    )
    .await??;
    let result = response.result.context("owned process result")?;
    assert_ne!(result["isError"], json!(true), "{result}");
    let owner = result["structuredContent"]["processId"]
        .as_str()
        .context("owned process ID")?
        .to_owned();
    let owned = store
        .create(&CreateJobParams::shell(
            "owned session job".into(),
            chaos_cron::Schedule::Interval { seconds: 300 }.to_json(),
            "echo owned".into(),
            CronScope::Session,
            None,
            Some(owner.clone()),
        ))
        .await?;
    assert_eq!(store.list(None, None).await?.len(), 3);

    let request_id = mcp
        .send_custom_request("resources/read", Some(json!({ "uri": "chaos://crons" })))
        .await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(resp) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };
    assert_eq!(resp.id, Some(request_id.to_value()));
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);

    let text = resp.result.as_ref().unwrap()["contents"][0]["text"]
        .as_str()
        .expect("cron resource text");
    let crons: serde_json::Value = serde_json::from_str(text)?;
    let items = crons.as_array().expect("cron list array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], owned.id);
    assert_eq!(items[0]["name"], json!("owned session job"));
    assert_eq!(items[0]["scope"], json!("session"));
    assert_eq!(items[0]["session_id"], owner);
    assert!(!text.contains("persisted job"));
    assert!(!text.contains("foreign-private-command"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spool_resource_never_exposes_global_rows_without_a_trustworthy_owner() -> Result<()> {
    let chaos_home = TempDir::new()?;

    let pool = open_runtime_db(chaos_home.path()).await?;
    sqlx::query(
        "INSERT INTO spool_jobs \
         (manifest_id, backend, batch_id, status, request_count, payload_json, \
          submitted_at, created_at, updated_at) \
         VALUES (?, 'xai', 'batch-1', 'InProgress', 2, \
         '[\"foreign-private-prompt\"]', 123, 111, 222)",
    )
    .bind("manifest-1")
    .execute(&pool)
    .await?;

    let mut mcp = McpProcess::new(chaos_home.path()).await?;

    mcp.initialize().await?;

    let request_id = mcp
        .send_custom_request("resources/read", Some(json!({ "uri": "chaos://spool" })))
        .await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(resp) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };
    assert_eq!(resp.id, Some(request_id.to_value()));
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);

    let text = resp.result.as_ref().unwrap()["contents"][0]["text"]
        .as_str()
        .expect("spool resource text");
    let spool: serde_json::Value = serde_json::from_str(text)?;
    let items = spool.as_array().expect("spool list array");
    assert!(items.is_empty());
    assert!(!text.contains("foreign-private-prompt"));
    assert!(!text.contains("manifest-1"));
    let persisted: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM spool_jobs")
        .fetch_one(&pool)
        .await?;
    assert_eq!(persisted, 1, "resource reads must not delete private rows");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resource_subscribe_accepts_only_chaos_mcp() -> Result<()> {
    let (_chaos_home, mut mcp) = spawn_mcp_process().await?;
    mcp.initialize().await?;

    let cases = [
        (
            "file:///tmp/test.txt",
            Some((
                ErrorCode::RESOURCE_NOT_FOUND,
                "Resource not found: file:///tmp/test.txt",
            )),
        ),
        (
            "chaos://models",
            Some((
                ErrorCode::INVALID_PARAMS,
                "Resource does not support subscriptions",
            )),
        ),
        ("chaos://mcp", None),
    ];
    for (uri, expected_error) in cases {
        let request_id = mcp
            .send_custom_request("resources/subscribe", Some(json!({ "uri": uri })))
            .await?;
        let message = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_response_or_error_message(request_id.clone()),
        )
        .await??;

        let JsonRpcMessage::Response(resp) = message else {
            anyhow::bail!("expected JSON-RPC response, got: {message:?}");
        };
        assert_eq!(resp.id, Some(request_id.to_value()));
        let error = resp
            .error
            .as_ref()
            .map(|error| (error.code, error.message.as_str()));
        assert_eq!(error, expected_error, "{uri}");
    }

    Ok(())
}
