//! Loopback fixtures only: no live credentials, provider or subprocess.
use super::*;
use crate::client::ModelClient;
use crate::invocation::{InvocationAccountingError, ProviderInvocationSink};
use chaos_journald::{InvocationReceipt, InvocationStart};
use futures::future::BoxFuture;
use serde_json::Value;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[derive(Debug, Default)]
struct Sink {
    reject_begin: AtomicBool,
    reject_receipt: AtomicBool,
    starts: Mutex<Vec<InvocationStart>>,
    receipts: Mutex<Vec<InvocationReceipt>>,
}
impl ProviderInvocationSink for Sink {
    fn begin(
        &self,
        start: InvocationStart,
    ) -> BoxFuture<'_, Result<bool, InvocationAccountingError>> {
        Box::pin(async move {
            self.starts.lock().unwrap().push(start);
            Ok(!self.reject_begin.load(Ordering::SeqCst))
        })
    }
    fn complete(
        &self,
        _: uuid::Uuid,
        receipt: InvocationReceipt,
    ) -> BoxFuture<'_, Result<(), InvocationAccountingError>> {
        Box::pin(async move {
            if self.reject_receipt.load(Ordering::SeqCst) {
                return Err(InvocationAccountingError);
            }
            self.receipts.lock().unwrap().push(receipt);
            Ok(())
        })
    }
}

fn sse() -> String {
    [
        json!({"type":"response.created","response":{"id":"sample"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{
            "type":"message","id":"m","role":"assistant","content":[]
        }}),
        json!({"type":"response.output_text.delta","item_id":"m","output_index":0,
            "content_index":0,"delta":"bounded answer"}),
        json!({"type":"response.output_item.done","output_index":0,"item":{
            "type":"message","id":"m","role":"assistant",
            "content":[{"type":"output_text","text":"bounded answer"}]
        }}),
        json!({"type":"response.completed","response":{"id":"sample","usage":{
            "input_tokens":3,"output_tokens":2,"total_tokens":5
        }}}),
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect()
}

async fn setup(server: &MockServer, sink: Arc<Sink>) -> Caller {
    let (mut session, mut turn) = crate::chaos::make_session_and_context().await;
    turn.sub_id = uuid::Uuid::new_v4().to_string();
    session.permission_actor.register_turn(&turn).await.unwrap();
    turn.model_info.native_server_side_tools = vec!["web_search".into()];
    let mut provider = crate::model_provider_info::create_oss_provider_with_base_url(
        &format!("{}/v1", server.uri()),
        crate::model_provider_info::WireApi::Responses,
    );
    provider.request_max_retries = Some(0);
    session.services.model_client = ModelClient::new(
        None,
        session.conversation_id,
        "fixture-sample".into(),
        provider,
        chaos_ipc::protocol::SessionSource::Exec,
        chaos_ipc::protocol::ApprovalPolicy::Headless,
        None,
        false,
        None,
        false,
        crate::config::ClampSettings::default(),
    );
    session
        .services
        .model_client
        .set_invocation_sink(sink)
        .unwrap();
    let session = Arc::new(session);
    session
        .services
        .tool_group_catalog
        .set_groups_enabled(
            &session.services.tool_group_state,
            [crate::tools::groups::WORKFLOWS],
            true,
        )
        .unwrap();
    Caller {
        invocation: Arc::new(crate::tools::workflows::tests::invocation(
            session,
            Arc::new(turn),
            Arc::new(tokio::sync::Mutex::new(
                crate::turn_diff_tracker::TurnDiffTracker::new(),
            )),
            "run_workflow",
            json!({}),
        )),
        cancellation: tokio_util::sync::CancellationToken::new(),
        run_id: serde_json::from_value(json!(uuid::Uuid::new_v4())).unwrap(),
        tasks: tokio_util::task::TaskTracker::new(),
    }
}

async fn mount(server: &MockServer) {
    mount_body(server, sse()).await;
}

async fn mount_body(server: &MockServer, body: String) {
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn native_sample_tree_uses_bounded_tool_free_metered_provider_request() {
    use crate::tools::registry::ToolHandler;
    let server = MockServer::start().await;
    mount(&server).await;
    let sink = Arc::new(Sink::default());
    let caller = setup(&server, sink.clone()).await;
    let mut invocation = (*caller.invocation).clone();
    invocation.payload = crate::tools::context::ToolPayload::Function {
        arguments: json!({"tree":{"type":"sequence","id":"sample_tree","children":[
            {"type":"sample","id":"sample","prompt":"Explicit prompt only",
             "system":"Write a short answer","max_tokens":32},
            {"type":"condition","id":"check","pointer":"/sample",
             "predicate":{"op":"equals","value":"bounded answer"}}
        ]}})
        .to_string(),
    };
    let result = crate::tools::workflows::WorkflowHandler
        .handle(invocation)
        .await
        .unwrap();
    let snapshot: Value = serde_json::from_str(&result.into_text()).unwrap();
    assert_eq!(snapshot["status"]["outcome"], "success", "{snapshot}");
    assert_eq!(snapshot["steps"][0]["detail"]["reply"], "bounded answer");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(request["max_output_tokens"], 32);
    assert!(
        request["tools"].as_array().is_none_or(Vec::is_empty),
        "{request}"
    );
    assert_eq!(request["parallel_tool_calls"], false);
    assert_eq!(request["input"].as_array().unwrap().len(), 1);
    assert!(request.get("previous_response_id").is_none());
    assert_eq!(sink.starts.lock().unwrap().len(), 1);
    assert_eq!(
        sink.starts.lock().unwrap()[0].turn_id.to_string(),
        caller.invocation.turn.sub_id,
    );
    let receipts = sink.receipts.lock().unwrap();
    assert_eq!(receipts.len(), 1);
    assert!(receipts[0].completed);
    assert_eq!(receipts[0].usage.as_ref().unwrap().total_tokens, 5);
}

#[tokio::test]
async fn provider_incomplete_is_not_fabricated_as_sampling_success() {
    let server = MockServer::start().await;
    let delta = json!({"type":"response.output_text.delta","item_id":"m",
        "output_index":0,"content_index":0,"delta":"partial"});
    let incomplete = json!({"type":"response.incomplete","response":{
        "id":"partial","incomplete_details":{"reason":"max_output_tokens"}
    }});
    mount_body(&server, format!("data: {delta}\n\ndata: {incomplete}\n\n")).await;
    let sink = Arc::new(Sink::default());
    let caller = setup(&server, sink.clone()).await;
    let error = crate::tools::workflows::sampling::sample(&caller, params())
        .await
        .err()
        .unwrap();
    assert!(error.contains("Incomplete response"), "{error}");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert_eq!(sink.receipts.lock().unwrap().len(), 1);
    assert!(!sink.receipts.lock().unwrap()[0].completed);
}

#[tokio::test]
async fn native_sample_never_bypasses_admission_or_receipt_barrier() {
    for reject_begin in [true, false] {
        let server = MockServer::start().await;
        mount(&server).await;
        let sink = Arc::new(Sink::default());
        sink.reject_begin.store(reject_begin, Ordering::SeqCst);
        sink.reject_receipt.store(!reject_begin, Ordering::SeqCst);
        let caller = setup(&server, sink.clone()).await;
        let result = crate::tools::workflows::sampling::sample(&caller, params()).await;
        assert!(
            result.is_err(),
            "rejected accounting produced a successful sample"
        );
        assert_eq!(sink.starts.lock().unwrap().len(), 1);
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            if reject_begin { 0 } else { 1 }
        );
        assert!(sink.receipts.lock().unwrap().is_empty());
        assert_eq!(
            caller
                .invocation
                .session
                .state
                .lock()
                .await
                .token_info()
                .unwrap()
                .total_token_usage
                .total_tokens,
            0
        );
    }
}
