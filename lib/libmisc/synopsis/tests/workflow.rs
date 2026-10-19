//! Execute compiled definitions with the real SDK dispatcher, not a second runner.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chaos_synopsis::tree::{TreeErrorCode, TreeNode, compile_workflow};
use mcp_host::bt::TreeTool;
use mcp_host::prelude::{ExecutionContext, Server, ToolError, ToolOutput};
use mcp_host::protocol::types::JsonRpcRequest;
use mcp_host::registry::tools::{Tool, ToolFuture};
use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Probe {
    calls: Arc<AtomicUsize>,
    failures: usize,
}

impl Tool for Probe {
    fn name(&self) -> &str {
        "probe"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn execute<'a>(&'a self, ctx: ExecutionContext<'a>) -> ToolFuture<'a> {
        Box::pin(async move {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= self.failures {
                return Err(ToolError::Execution("probe failed".into()));
            }
            if ctx.params["pending"] == true {
                return futures::future::pending().await;
            }
            ToolOutput::structured(ctx.params)
                .map_err(|error| ToolError::Execution(error.to_string()))
        })
    }
}

async fn execute(definition: Value, input: Value, failures: usize) -> TestResult<(Value, usize)> {
    let root = compile_workflow(serde_json::from_value(definition)?, &|tool| tool == "probe")?;
    let server = Server::builder("workflow-test", "1")
        .with_tools(true)
        .build();
    let calls = Arc::new(AtomicUsize::new(0));
    server.tool_registry().register(Probe {
        calls: calls.clone(),
        failures,
    })?;
    server
        .tool_registry()
        .register(TreeTool::new("workflow", root)?)?;
    let initialized = server
        .handle_request(
            "owner",
            JsonRpcRequest::new(
                json!(1),
                "initialize",
                Some(json!({
                    "protocolVersion": "2025-11-25", "capabilities": {},
                    "clientInfo": {"name": "workflow-test", "version": "1"}
                })),
            ),
        )
        .await
        .ok_or("missing initialize response")?;
    assert!(initialized.error.is_none(), "{initialized:?}");
    server
        .handle_request(
            "owner",
            JsonRpcRequest::notification("notifications/initialized", None),
        )
        .await;
    let response = server
        .handle_request(
            "owner",
            JsonRpcRequest::new(
                json!(2),
                "tools/call",
                Some(json!({"name": "workflow", "arguments": input})),
            ),
        )
        .await
        .ok_or("missing workflow response")?;
    assert!(response.error.is_none(), "{response:?}");
    Ok((
        response.result.ok_or("missing workflow result")?,
        calls.load(Ordering::SeqCst),
    ))
}

#[tokio::test]
async fn bindings_and_conditions_read_prior_structured_results() -> TestResult {
    let (result, calls) = execute(
        json!({
            "type": "sequence", "id": "root", "children": [
                {"type":"call", "id":"first", "tool":"probe", "arguments":{"literal":true},
                 "bindings":[{"source":"/input/value", "target":"/value"}], "result_key":"answer"},
                {
                    "type":"condition", "id":"check", "pointer":"/answer/value",
                    "predicate":{"op":"equals", "value":42}
                },
                {"type":"call", "id":"second", "tool":"probe", "arguments":{},
                 "bindings":[{"source":"/answer", "target":""}]}
            ]
        }),
        json!({"value":42}),
        0,
    )
    .await?;
    assert!(!result["isError"].as_bool().unwrap_or(false), "{result}");
    assert_eq!(calls, 2);
    let steps = &result["structuredContent"]["steps"];
    assert_eq!(steps[0]["id"], "first");
    assert_eq!(steps[0]["result"], json!({"literal":true,"value":42}));
    assert_eq!(steps[1]["id"], "check");
    assert_eq!(steps[2]["result"], steps[0]["result"]);
    Ok(())
}

#[tokio::test]
async fn missing_bindings_fail_before_dispatch_and_fallback_can_recover() -> TestResult {
    let (result, calls) = execute(
        json!({
            "type":"fallback", "id":"root", "children":[
                {"type":"call", "id":"missing", "tool":"probe", "arguments":{},
                 "bindings":[{"source":"/input/absent", "target":"/value"}]},
                {"type":"call", "id":"recover", "tool":"probe", "arguments":{"safe":true}}
            ]
        }),
        json!({}),
        0,
    )
    .await?;
    assert_eq!(calls, 1);
    let steps = &result["structuredContent"]["steps"];
    let diagnostic: Value = serde_json::from_str(
        steps[0]["error"]
            .as_str()
            .ok_or("missing binding diagnostic")?,
    )?;
    assert_eq!(diagnostic["binding_error"]["code"], "missing_source");
    assert_eq!(steps[1]["result"], json!({"safe":true}));
    Ok(())
}

#[tokio::test]
async fn retries_use_total_attempts_and_stable_leaf_ids() -> TestResult {
    let (result, calls) = execute(
        json!({
            "type":"retry", "id":"retry", "attempts":3,
            "child":{"type":"call", "id":"attempt", "tool":"probe", "arguments":{}}
        }),
        json!({}),
        2,
    )
    .await?;
    assert_eq!(calls, 3);
    let steps = result["structuredContent"]["steps"]
        .as_array()
        .ok_or("missing steps")?;
    assert_eq!(steps.len(), 3);
    for (index, step) in steps.iter().enumerate() {
        assert_eq!(step["id"], "attempt");
        assert_eq!(step["attempt"], index + 1);
    }
    Ok(())
}

#[tokio::test]
async fn unsupported_interactions_fail_without_fabricated_capabilities() -> TestResult {
    let (result, calls) = execute(
        json!({
            "type":"fallback", "id":"root", "children":[
                {"type":"ask", "id":"ask", "message":"more input?"},
                {"type":"sample", "id":"sample", "prompt":"summarize"},
                {"type":"call", "id":"recover", "tool":"probe", "arguments":{}}
            ]
        }),
        json!({}),
        0,
    )
    .await?;
    assert_eq!(calls, 1);
    let steps = &result["structuredContent"]["steps"];
    assert_eq!(steps[0]["id"], "ask");
    assert_eq!(steps[1]["id"], "sample");
    assert!(steps[0]["error"].is_string());
    assert!(steps[1]["error"].is_string());
    assert_eq!(steps[2]["id"], "recover");
    Ok(())
}

#[test]
fn complete_preflight_precedes_compilation() -> TestResult {
    for (definition, code) in [
        (
            json!({"type":"retry", "id":"retry", "attempts":2,
            "child":{"type":"call", "id":"call", "tool":"probe", "arguments":{}}}),
            TreeErrorCode::UnsafeRetry,
        ),
        (
            json!({"type":"sequence", "id":"root", "children":[
                {"type":"call", "id":"safe", "tool":"probe", "arguments":{}},
                {"type":"call", "id":"late", "tool":"run_workflow", "arguments":{}}
            ]}),
            TreeErrorCode::ForbiddenTool,
        ),
        (
            json!({"type":"job", "id":"agent", "job_id":"job"}),
            TreeErrorCode::WrongDomain,
        ),
    ] {
        let root: TreeNode = serde_json::from_value(definition)?;
        let error = compile_workflow(root, &|_| false)
            .err()
            .ok_or("unexpected successful compile")?;
        assert_eq!(error.code, code);
    }
    Ok(())
}

#[tokio::test]
async fn composites_preserve_first_terminal_race_and_all_success_semantics() -> TestResult {
    for (kind, first, second, success) in [
        ("parallel_all", true, true, true),
        ("parallel_all", true, false, false),
        ("race", false, true, false),
        ("race", true, false, true),
    ] {
        let (result, calls) = execute(
            json!({
                "type":kind, "id":"root", "children":[
                    {
                        "type":"condition", "id":"left", "pointer":"/input/value",
                        "predicate":{"op":"equals", "value":first}
                    },
                    {
                        "type":"condition", "id":"right", "pointer":"/input/value",
                        "predicate":{"op":"equals", "value":second}
                    }
                ]
            }),
            json!({"value":true}),
            0,
        )
        .await?;
        assert_eq!(calls, 0);
        assert_eq!(
            !result["isError"].as_bool().unwrap_or(false),
            success,
            "{result}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn decorator_timeout_ends_pending_call_before_fallback() -> TestResult {
    let (result, calls) = tokio::time::timeout(std::time::Duration::from_secs(2), execute(json!({
        "type":"fallback", "id":"root", "children":[
            {"type":"timeout", "id":"timeout", "timeout_ms":1,
             "child":{"type":"call", "id":"pending", "tool":"probe", "arguments":{"pending":true}}},
            {"type":"call", "id":"recover", "tool":"probe", "arguments":{"recovered":true}}
        ]
    }), json!({}), 0)).await??;
    assert_eq!(calls, 2);
    assert!(!result["isError"].as_bool().unwrap_or(false), "{result}");
    let steps = result["structuredContent"]["steps"]
        .as_array()
        .ok_or("missing steps")?;
    assert!(
        steps
            .iter()
            .any(|step| step["id"] == "recover" && step["result"]["recovered"] == true)
    );
    Ok(())
}
