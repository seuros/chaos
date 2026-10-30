use super::*;
use crate::invocation::InvocationAccountingError;
use rama::futures::future::BoxFuture;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

#[derive(Debug, Default)]
struct Sink {
    reject_begin: AtomicBool,
    reject_receipt: AtomicBool,
    starts: Mutex<Vec<InvocationStart>>,
    receipts: Mutex<Vec<(uuid::Uuid, InvocationReceipt)>>,
    receipt_entered: Notify,
    receipt_release: Notify,
    block_receipt: AtomicBool,
}
impl ProviderInvocationSink for Sink {
    fn begin(
        &self,
        start: InvocationStart,
    ) -> BoxFuture<'_, std::result::Result<bool, InvocationAccountingError>> {
        Box::pin(async move {
            self.starts.lock().unwrap().push(start);
            Ok(!self.reject_begin.load(Ordering::SeqCst))
        })
    }
    fn complete(
        &self,
        id: uuid::Uuid,
        receipt: InvocationReceipt,
    ) -> BoxFuture<'_, std::result::Result<(), InvocationAccountingError>> {
        Box::pin(async move {
            self.receipt_entered.notify_one();
            if self.block_receipt.load(Ordering::SeqCst) {
                self.receipt_release.notified().await;
            }
            if self.reject_receipt.load(Ordering::SeqCst) {
                return Err(InvocationAccountingError);
            }
            self.receipts.lock().unwrap().push((id, receipt));
            Ok(())
        })
    }
}

fn response(usage: Option<chaos_ipc::protocol::TokenUsage>) -> Result<ResponseEvent> {
    Ok(ResponseEvent::Completed {
        response_id: "provider-receipt".into(),
        token_usage: usage,
    })
}

#[tokio::test]
async fn completed_usage_cannot_pass_the_durable_receipt_barrier_or_be_counted_twice() {
    let sink = Arc::new(Sink::default());
    sink.block_receipt.store(true, Ordering::SeqCst);
    let id = uuid::Uuid::new_v4();
    let (tx, rx_event) = tokio::sync::mpsc::channel(4);
    let usage = chaos_ipc::protocol::TokenUsage {
        input_tokens: 19,
        output_tokens: 7,
        total_tokens: 26,
        ..Default::default()
    };
    tx.send(response(Some(usage.clone()))).await.unwrap();
    tx.send(response(Some(usage))).await.unwrap();
    let mut stream = account_response_stream(ResponseStream { rx_event }, sink.clone(), id);
    sink.receipt_entered.notified().await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), stream.next())
            .await
            .is_err()
    );
    sink.receipt_release.notify_one();
    assert!(matches!(
        stream.next().await,
        Some(Ok(ResponseEvent::Completed { .. }))
    ));
    assert!(stream.next().await.is_none());
    let receipts = sink.receipts.lock().unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].0, id);
    assert_eq!(receipts[0].1.usage.as_ref().unwrap().input_tokens, 19);
}

#[tokio::test]
async fn missing_usage_cancellation_and_failed_receipt_never_manufacture_zero_or_completion() {
    for mode in 0..3 {
        let sink = Arc::new(Sink::default());
        sink.reject_receipt.store(mode == 2, Ordering::SeqCst);
        let (tx, rx_event) = tokio::sync::mpsc::channel(4);
        let mut stream = account_response_stream(
            ResponseStream { rx_event },
            sink.clone(),
            uuid::Uuid::new_v4(),
        );
        if mode == 1 {
            drop(stream);
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while sink.receipts.lock().unwrap().is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                sink.receipts.lock().unwrap()[0].1,
                InvocationReceipt::unknown()
            );
        } else {
            tx.send(response(None)).await.unwrap();
            if mode == 2 {
                assert!(matches!(stream.next().await, Some(Err(ChaosErr::Fatal(_)))));
                assert!(sink.receipts.lock().unwrap().is_empty());
            } else {
                assert!(matches!(
                    stream.next().await,
                    Some(Ok(ResponseEvent::Completed {
                        token_usage: None,
                        ..
                    }))
                ));
                assert!(sink.receipts.lock().unwrap()[0].1.usage.is_none());
            }
        }
    }
}

#[tokio::test]
async fn rejected_or_duplicate_begin_cannot_touch_the_provider_client() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = crate::model_provider_info::create_oss_provider_with_base_url(
        &format!("http://{}/v1", listener.local_addr().unwrap()),
        crate::model_provider_info::WireApi::Responses,
    );
    let client = ModelClient::new(
        None,
        chaos_ipc::ProcessId::new(),
        "test-provider".into(),
        provider,
        chaos_ipc::protocol::SessionSource::Api,
        chaos_ipc::protocol::ApprovalPolicy::Headless,
        None,
        false,
        None,
        false,
        crate::config::ClampSettings::default(),
    );
    let sink = Arc::new(Sink::default());
    sink.reject_begin.store(true, Ordering::SeqCst);
    client.set_invocation_sink(sink.clone()).unwrap();
    let (_, context) = crate::chaos::make_session_and_context().await;
    let mut session = client.new_session();
    assert!(
        session
            .stream_accounted(
                &uuid::Uuid::new_v4().to_string(),
                InvocationPurpose::Sampling,
                &Prompt::default(),
                &context.model_info,
                &context.session_telemetry,
                None,
                ReasoningSummary::None,
                None,
                None,
            )
            .await
            .is_err()
    );
    assert_eq!(sink.starts.lock().unwrap().len(), 1);
    assert!(sink.receipts.lock().unwrap().is_empty());
    assert!(
        session
            .stream(
                &Prompt::default(),
                &context.model_info,
                &context.session_telemetry,
                None,
                ReasoningSummary::None,
                None,
                None,
            )
            .await
            .is_err(),
        "plain streaming cannot bypass an installed sink"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn metered_errors_never_hide_transport_retries_auth_replays_or_wire_probes() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let (_, context) = crate::chaos::make_session_and_context().await;
    for (status, wire) in [
        (500, crate::model_provider_info::WireApi::Responses),
        (401, crate::model_provider_info::WireApi::Responses),
        (404, crate::model_provider_info::WireApi::Auto),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(status).set_body_string("not accepted"))
            .mount(&server)
            .await;
        // A successful fallback would conceal a second dispatch in one record.
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "data: {\"id\":\"hidden\",\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n",
            ))
            .mount(&server)
            .await;
        let mut provider = crate::model_provider_info::create_oss_provider_with_base_url(
            &format!("{}/v1", server.uri()),
            wire,
        );
        provider.request_max_retries = Some(4);
        let client = ModelClient::new(
            None,
            chaos_ipc::ProcessId::new(),
            "test-provider".into(),
            provider,
            chaos_ipc::protocol::SessionSource::Api,
            chaos_ipc::protocol::ApprovalPolicy::Headless,
            None,
            false,
            None,
            false,
            crate::config::ClampSettings::default(),
        );
        let sink = Arc::new(Sink::default());
        client.set_invocation_sink(sink.clone()).unwrap();
        let setup = client.current_client_setup().await.unwrap();
        assert_eq!(setup.api_provider.retry.max_attempts, 1);
        let mut session = client.new_session();
        for _ in 0..2 {
            let result = session
                .stream_accounted(
                    &uuid::Uuid::new_v4().to_string(),
                    InvocationPurpose::Sampling,
                    &Prompt::default(),
                    &context.model_info,
                    &context.session_telemetry,
                    None,
                    ReasoningSummary::None,
                    None,
                    None,
                )
                .await;
            assert!(result.is_err());
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2, "each logical retry needs its own record");
        assert!(requests.iter().all(|r| r.url.path() == "/v1/responses"));
        let starts = sink.starts.lock().unwrap();
        assert_eq!(starts.len(), 2);
        assert_ne!(starts[0].invocation_id, starts[1].invocation_id);
        let receipts = sink.receipts.lock().unwrap();
        assert_eq!(receipts.len(), 2);
        assert!(
            receipts
                .iter()
                .all(|(_, r)| r == &InvocationReceipt::unknown())
        );
    }
}

#[derive(Debug)]
struct LineageSink {
    process_id: chaos_ipc::ProcessId,
    reject_fork: bool,
    edges: Arc<Mutex<Vec<(uuid::Uuid, chaos_ipc::ProcessId, chaos_ipc::ProcessId)>>>,
    starts: Arc<Mutex<Vec<InvocationStart>>>,
    started: Arc<Notify>,
}

impl LineageSink {
    fn new(process_id: chaos_ipc::ProcessId, reject_fork: bool) -> Self {
        Self {
            process_id,
            reject_fork,
            edges: Arc::default(),
            starts: Arc::default(),
            started: Arc::default(),
        }
    }
}

impl ProviderInvocationSink for LineageSink {
    fn fork(
        &self,
        turn: uuid::Uuid,
        parent: chaos_ipc::ProcessId,
        child: chaos_ipc::ProcessId,
    ) -> BoxFuture<
        '_,
        std::result::Result<Arc<dyn ProviderInvocationSink>, InvocationAccountingError>,
    > {
        Box::pin(async move {
            assert_eq!(parent, self.process_id);
            assert_ne!(child, parent);
            self.edges.lock().unwrap().push((turn, parent, child));
            if self.reject_fork {
                return Err(InvocationAccountingError);
            }
            Ok(Arc::new(Self {
                process_id: child,
                reject_fork: false,
                edges: self.edges.clone(),
                starts: self.starts.clone(),
                started: self.started.clone(),
            }) as Arc<dyn ProviderInvocationSink>)
        })
    }

    fn begin(
        &self,
        request: InvocationStart,
    ) -> BoxFuture<'_, std::result::Result<bool, InvocationAccountingError>> {
        Box::pin(async move {
            assert_eq!(request.process_id, self.process_id);
            self.starts.lock().unwrap().push(request);
            self.started.notify_one();
            Ok(false)
        })
    }

    fn complete(
        &self,
        _: uuid::Uuid,
        _: InvocationReceipt,
    ) -> BoxFuture<'_, std::result::Result<(), InvocationAccountingError>> {
        Box::pin(async { panic!("rejected dispatch cannot produce a receipt") })
    }
}

#[tokio::test]
async fn review_delegate_cannot_start_unmetered_or_claim_its_parent_process_identity() {
    for reject in [true, false] {
        let (mut parent, mut context) = crate::chaos::make_session_and_context().await;
        context.sub_id = uuid::Uuid::new_v4().to_string();
        let manager = crate::ProcessTable::with_models_provider_and_home_for_tests(
            crate::ChaosAuth::from_api_key("unused"),
            context.config.model_provider.clone(),
            context.config.chaos_home.clone(),
        );
        parent.services.agent_control = manager.agent_control();
        let sink = Arc::new(LineageSink::new(parent.conversation_id, reject));
        parent
            .services
            .model_client
            .set_invocation_sink(sink.clone())
            .unwrap();
        let (_, status) = tokio::sync::watch::channel(crate::minions::AgentStatus::Completed(None));
        let parent = manager
            .insert_session_for_tests(parent, &context, status)
            .await;
        let parent = parent.chaos.session.clone();
        let context = Arc::new(context);
        let cancel = tokio_util::sync::CancellationToken::new();
        let delegate = crate::chaos_delegate::run_chaos_process_interactive(
            context.config.as_ref().clone(),
            parent.services.auth_manager.clone(),
            parent.services.models_manager.clone(),
            parent.clone(),
            context.clone(),
            cancel.clone(),
            chaos_ipc::protocol::SubAgentSource::Review,
            None,
        )
        .await;
        let edge = sink.edges.lock().unwrap()[0];
        assert_eq!(edge.0.to_string(), context.sub_id);
        assert_eq!(edge.1, parent.conversation_id);
        if reject {
            assert!(
                delegate.is_err(),
                "failed lineage must abort child initialization"
            );
        } else {
            let delegate = delegate.unwrap();
            assert_eq!(delegate.session.conversation_id, edge.2);
            assert!(manager.get_process(edge.2).await.is_ok());
            delegate
                .session
                .services
                .internal_task_store
                .set_active(true)
                .await;
            assert!(
                !manager
                    .process_tree_is_quiescent(parent.conversation_id)
                    .await
                    .unwrap()
            );
            delegate
                .session
                .services
                .internal_task_store
                .set_active(false)
                .await;
            assert!(
                delegate
                    .session
                    .services
                    .model_client
                    .has_invocation_sink()
                    .unwrap()
            );
            let local_turn = uuid::Uuid::new_v4();
            let mut client = delegate.session.services.model_client.new_session();
            assert!(
                client
                    .stream_accounted(
                        &local_turn.to_string(),
                        InvocationPurpose::Sampling,
                        &Prompt::default(),
                        &context.model_info,
                        &context.session_telemetry,
                        None,
                        ReasoningSummary::None,
                        None,
                        None,
                    )
                    .await
                    .is_err()
            );
            {
                let starts = sink.starts.lock().unwrap();
                assert_eq!(starts.len(), 1);
                assert_eq!(starts[0].process_id, edge.2);
                assert_eq!(starts[0].turn_id, local_turn);
            }
            delegate
                .submit(chaos_ipc::protocol::Op::Shutdown {})
                .await
                .unwrap();
        }
        cancel.cancel();
    }
}

struct ParentTurn;

impl crate::tasks::SessionTask for ParentTurn {
    fn kind(&self) -> crate::state::TaskKind {
        crate::state::TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "invocation_lineage_test_parent"
    }

    fn run(
        self: Arc<Self>,
        _: Arc<crate::tasks::SessionTaskContext>,
        _: Arc<crate::chaos::TurnContext>,
        _: Vec<chaos_ipc::user_input::UserInput>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> {
        Box::pin(async move {
            cancel.cancelled().await;
            None
        })
    }
}

#[tokio::test]
async fn native_minions_require_live_lineage_and_cannot_adopt_persisted_child_history() {
    use chaos_ipc::protocol::{SessionSource, SubAgentSource, TurnAbortReason};

    let (mut session, mut context) = crate::chaos::make_session_and_context().await;
    context.sub_id = uuid::Uuid::new_v4().to_string();
    let manager = crate::ProcessTable::with_models_provider_and_home_for_tests(
        crate::ChaosAuth::from_api_key("unused"),
        context.config.model_provider.clone(),
        context.config.chaos_home.clone(),
    );
    session.services.agent_control = manager.agent_control();
    let sink = Arc::new(LineageSink::new(session.conversation_id, false));
    session
        .services
        .model_client
        .set_invocation_sink(sink.clone())
        .unwrap();
    let (_, status) = tokio::sync::watch::channel(crate::minions::AgentStatus::Completed(None));
    let parent = manager
        .insert_session_for_tests(session, &context, status)
        .await;
    let parent = &parent.chaos.session;
    let source = SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
        parent_process_id: parent.conversation_id,
        depth: 1,
        agent_nickname: None,
        agent_role: None,
    });
    let control = &parent.services.agent_control;
    assert!(
        control
            .spawn_agent(
                context.config.as_ref().clone(),
                vec![],
                Some(source.clone())
            )
            .await
            .is_err(),
        "an idle parent cannot authorize child work"
    );
    assert!(sink.edges.lock().unwrap().is_empty());
    parent
        .spawn_task(Arc::new(context), vec![], ParentTurn)
        .await;
    let config = parent.get_config().await;
    let resumed = control
        .resume_agent_from_rollout(
            config.as_ref().clone(),
            chaos_ipc::ProcessId::new(),
            source.clone(),
        )
        .await;
    assert!(matches!(resumed, Err(ChaosErr::InvalidRequest(_))));
    assert!(sink.edges.lock().unwrap().is_empty());
    let child_id = control
        .spawn_agent(
            config.as_ref().clone(),
            vec![chaos_ipc::user_input::UserInput::Text {
                text: "metered descendant".into(),
                text_elements: vec![],
            }],
            Some(source.clone()),
        )
        .await
        .unwrap();
    let child = manager.get_process(child_id).await.unwrap();
    assert!(
        child
            .chaos
            .session
            .services
            .model_client
            .has_invocation_sink()
            .unwrap()
    );
    assert_eq!(sink.edges.lock().unwrap()[0].2, child_id);
    parent.abort_all_tasks(TurnAbortReason::Interrupted).await;
    let _ = control.shutdown_agent(child_id).await;
    manager.remove_process(&parent.conversation_id).await;
    assert!(
        matches!(
            control
                .spawn_agent(config.as_ref().clone(), vec![], Some(source))
                .await,
            Err(ChaosErr::ProcessNotFound(_))
        ),
        "a removed metered parent cannot silently become an unmetered historical parent"
    );
}

#[tokio::test]
async fn unregistered_descendant_never_dispatches_and_dropped_activation_discards_queued_work() {
    for activate in [false, true] {
        let (parent, mut context) = crate::chaos::make_session_and_context().await;
        context.sub_id = uuid::Uuid::new_v4().to_string();
        let sink = Arc::new(LineageSink::new(parent.conversation_id, false));
        parent
            .services
            .model_client
            .set_invocation_sink(sink.clone())
            .unwrap();
        let invocation_parent = parent
            .services
            .model_client
            .invocation_parent(&context.sub_id)
            .unwrap();
        let (activation, submission_activation) = tokio::sync::oneshot::channel();
        let started = crate::chaos::Chaos::spawn(crate::chaos::ChaosSpawnArgs {
            config: context.config.as_ref().clone(),
            auth_manager: parent.services.auth_manager.clone(),
            models_manager: parent.services.models_manager.clone(),
            mcp_manager: parent.services.mcp_manager.clone(),
            file_watcher: parent.services.file_watcher.clone(),
            conversation_history: chaos_ipc::protocol::InitialHistory::New,
            session_source: chaos_ipc::protocol::SessionSource::SubAgent(
                chaos_ipc::protocol::SubAgentSource::Review,
            ),
            agent_control: parent.services.agent_control.clone(),
            dynamic_tools: vec![],
            persist_extended_history: false,
            metrics_service_name: None,
            inherited_shell_environment: None,
            parent_trace: None,
            invocation_parent,
            submission_activation: Some(submission_activation),
        })
        .await
        .unwrap();
        let child = started.chaos;
        child
            .submit(
                child
                    .user_turn(
                        vec![chaos_ipc::user_input::UserInput::Text {
                            text: "must wait for registry ownership".into(),
                            text_elements: vec![],
                        }],
                        None,
                    )
                    .await,
            )
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(sink.starts.lock().unwrap().is_empty());
        assert!(
            child
                .session
                .services
                .model_client
                .has_pending_submissions()
        );
        if activate {
            activation.send(()).unwrap();
            // This crosses native turn preparation, not just channel delivery.
            // Wait for the actual begin barrier without spinning the runtime;
            // the outer nextest budget still catches a genuinely stuck child.
            // notify_one retains a permit if begin wins the scheduling race.
            tokio::time::timeout(std::time::Duration::from_secs(30), sink.started.notified())
                .await
                .expect("activated descendant did not reach the invocation begin barrier");
        } else {
            drop(activation);
        }
        child.shutdown_and_wait().await.unwrap();
        assert_eq!(sink.starts.lock().unwrap().len(), usize::from(activate));
        assert!(
            !child
                .session
                .services
                .model_client
                .has_pending_submissions()
        );
    }
}

#[tokio::test]
async fn authority_lookup_is_not_an_untracked_gap_before_pending_boot_registration() {
    let (mut session, context) = crate::chaos::make_session_and_context().await;
    let manager = crate::ProcessTable::with_models_provider_and_home_for_tests(
        crate::ChaosAuth::from_api_key("unused"),
        context.config.model_provider.clone(),
        context.config.chaos_home.clone(),
    );
    session.services.agent_control = manager.agent_control();
    let sink = Arc::new(LineageSink::new(session.conversation_id, true));
    session
        .services
        .model_client
        .set_invocation_sink(sink.clone())
        .unwrap();
    let (_, status) = tokio::sync::watch::channel(crate::minions::AgentStatus::Completed(None));
    let parent = manager
        .insert_session_for_tests(session, &context, status)
        .await;
    let root = parent.chaos.session.conversation_id;
    assert!(manager.process_tree_is_quiescent(root).await.unwrap());
    // Stop the router inside authority lookup, before a pending boot exists.
    let active = parent.chaos.session.active_turn.lock().await;
    let control = parent.chaos.session.services.agent_control.clone();
    let config = context.config.as_ref().clone();
    let spawn = tokio::spawn(async move {
        control
            .spawn_agent(
                config,
                vec![],
                Some(chaos_ipc::protocol::SessionSource::SubAgent(
                    chaos_ipc::protocol::SubAgentSource::ProcessSpawn {
                        parent_process_id: root,
                        depth: 1,
                        agent_nickname: None,
                        agent_role: None,
                    },
                )),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while manager.process_tree_is_quiescent(root).await.unwrap() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(sink.edges.lock().unwrap().is_empty());
    drop(active);
    assert!(spawn.await.unwrap().is_err());
    assert!(manager.process_tree_is_quiescent(root).await.unwrap());
}
