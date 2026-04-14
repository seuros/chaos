use super::*;

#[tokio::test]
async fn spawn_task_turn_span_inherits_dispatch_trace_context() {
    struct TraceCaptureTask {
        captured_trace: Arc<std::sync::Mutex<Option<W3cTraceContext>>>,
    }

    impl SessionTask for TraceCaptureTask {
        fn kind(&self) -> TaskKind {
            TaskKind::Regular
        }

        fn span_name(&self) -> &'static str {
            "session_task.trace_capture"
        }

        fn run(
            self: Arc<Self>,
            _session: Arc<SessionTaskContext>,
            _ctx: Arc<TurnContext>,
            _input: Vec<UserInput>,
            _cancellation_token: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> {
            Box::pin(async move {
                let mut trace = self
                    .captured_trace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *trace = current_span_w3c_trace_context();
                None
            })
        }
    }

    init_test_tracing();

    let request_parent = W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000011-0000000000000022-01".into()),
        tracestate: Some("vendor=value".into()),
    };
    let request_span = tracing::info_span!("app_server.request");
    assert!(set_parent_from_w3c_trace_context(
        &request_span,
        &request_parent
    ));

    let submission_trace =
        async { current_span_w3c_trace_context().expect("request span should have trace context") }
            .instrument(request_span)
            .await;

    let dispatch_span = submission_dispatch_span(&Submission {
        id: "sub-1".into(),
        op: Op::Interrupt,
        trace: Some(submission_trace.clone()),
    });
    let dispatch_span_id = dispatch_span.context().span().span_context().span_id();

    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let captured_trace = Arc::new(std::sync::Mutex::new(None));

    async {
        sess.spawn_task(
            Arc::clone(&tc),
            vec![UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            }],
            TraceCaptureTask {
                captured_trace: Arc::clone(&captured_trace),
            },
        )
        .await;
    }
    .instrument(dispatch_span)
    .await;

    let evt = tokio::time::timeout(StdDuration::from_secs(2), rx.recv())
        .await
        .expect("timeout waiting for turn completion")
        .expect("event");
    assert!(matches!(evt.msg, EventMsg::TurnComplete(_)));

    let task_trace = captured_trace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("turn task should capture the current span trace context");
    let submission_context =
        chaos_syslog::context_from_w3c_trace_context(&submission_trace).expect("submission");
    let task_context =
        chaos_syslog::context_from_w3c_trace_context(&task_trace).expect("task trace");

    assert_eq!(
        task_context.span().span_context().trace_id(),
        submission_context.span().span_context().trace_id()
    );
    assert_ne!(
        task_context.span().span_context().span_id(),
        dispatch_span_id
    );
}

#[tokio::test]
async fn shutdown_and_wait_allows_multiple_waiters() {
    let (session, _turn_context) = make_session_and_context().await;
    let (tx_sub, rx_sub) = async_channel::bounded(4);
    let (_tx_event, rx_event) = async_channel::unbounded();
    let (_agent_status_tx, agent_status) = watch::channel(AgentStatus::PendingInit);
    let session_loop_handle = tokio::spawn(async move {
        let shutdown: Submission = rx_sub.recv().await.expect("shutdown submission");
        assert_eq!(shutdown.op, Op::Shutdown);
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    });
    let chaos = Arc::new(Chaos {
        tx_sub,
        rx_event,
        agent_status,
        session: Arc::new(session),
        session_loop_termination: session_loop_termination_from_handle(session_loop_handle),
    });

    let waiter_1 = {
        let chaos = Arc::clone(&chaos);
        tokio::spawn(async move { chaos.shutdown_and_wait().await })
    };
    let waiter_2 = {
        let chaos = Arc::clone(&chaos);
        tokio::spawn(async move { chaos.shutdown_and_wait().await })
    };

    waiter_1
        .await
        .expect("first shutdown waiter join")
        .expect("first shutdown waiter");
    waiter_2
        .await
        .expect("second shutdown waiter join")
        .expect("second shutdown waiter");
}

#[tokio::test]
async fn shutdown_and_wait_waits_when_shutdown_is_already_in_progress() {
    let (session, _turn_context) = make_session_and_context().await;
    let (tx_sub, rx_sub) = async_channel::bounded(4);
    drop(rx_sub);
    let (_tx_event, rx_event) = async_channel::unbounded();
    let (_agent_status_tx, agent_status) = watch::channel(AgentStatus::PendingInit);
    let (shutdown_complete_tx, shutdown_complete_rx) = tokio::sync::oneshot::channel();
    let session_loop_handle = tokio::spawn(async move {
        let _ = shutdown_complete_rx.await;
    });
    let chaos = Arc::new(Chaos {
        tx_sub,
        rx_event,
        agent_status,
        session: Arc::new(session),
        session_loop_termination: session_loop_termination_from_handle(session_loop_handle),
    });

    let waiter = {
        let chaos = Arc::clone(&chaos);
        tokio::spawn(async move { chaos.shutdown_and_wait().await })
    };

    tokio::time::sleep(StdDuration::from_millis(10)).await;
    assert!(!waiter.is_finished());

    shutdown_complete_tx
        .send(())
        .expect("session loop should still be waiting to terminate");

    waiter
        .await
        .expect("shutdown waiter join")
        .expect("shutdown waiter");
}

#[tokio::test]
async fn record_model_warning_appends_user_message() {
    let (mut session, turn_context) = make_session_and_context().await;
    let features = crate::features::Features::with_defaults().into();
    session.features = features;

    session
        .record_model_warning("too many unified exec processes", &turn_context)
        .await;

    let history = session.clone_history().await;
    let history_items = history.raw_items();
    let last = history_items.last().expect("warning recorded");

    match last {
        ResponseItem::Message { role, content, .. } => {
            assert_eq!(role, "user");
            assert_eq!(
                content,
                &vec![ContentItem::InputText {
                    text: "Warning: too many unified exec processes".to_string(),
                }]
            );
        }
        other => panic!("expected user message, got {other:?}"),
    }
}

#[tokio::test]
async fn spawn_task_does_not_update_previous_turn_settings_for_non_run_turn_tasks() {
    let (sess, tc, _rx) = make_session_and_context_with_rx().await;
    sess.set_previous_turn_settings(None).await;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];

    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: true,
        },
    )
    .await;

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;
    assert_eq!(sess.previous_turn_settings().await, None);
}

#[tokio::test]
async fn build_settings_update_items_emits_environment_item_for_network_changes() {
    let (session, previous_context) = make_session_and_context().await;
    let previous_context = Arc::new(previous_context);
    let mut current_context = previous_context
        .with_model(
            previous_context.model_info.slug.clone(),
            &session.services.models_manager,
        )
        .await;

    let mut config = (*current_context.config).clone();
    let mut requirements = config.config_layer_stack.requirements().clone();
    requirements.network = Some(Sourced::new(
        NetworkConstraints {
            allowed_domains: Some(vec!["api.example.com".to_string()]),
            denied_domains: Some(vec!["blocked.example.com".to_string()]),
            ..Default::default()
        },
        RequirementSource::Unknown,
    ));
    let layers = config
        .config_layer_stack
        .get_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, true)
        .into_iter()
        .cloned()
        .collect();
    config.config_layer_stack = ConfigLayerStack::new(
        layers,
        requirements,
        config.config_layer_stack.requirements_toml().clone(),
    )
    .expect("rebuild config layer stack with network requirements");
    current_context.config = Arc::new(config);

    let reference_context_item = previous_context.to_turn_context_item();
    let update_items = session
        .build_settings_update_items(Some(&reference_context_item), &current_context)
        .await;

    let environment_update = update_items
        .iter()
        .find_map(|item| match item {
            ResponseItem::Message { role, content, .. } if role == "user" => {
                let [ContentItem::InputText { text }] = content.as_slice() else {
                    return None;
                };
                text.contains("<environment_context>").then_some(text)
            }
            _ => None,
        })
        .expect("environment update item should be emitted");
    assert!(environment_update.contains("<network enabled=\"true\">"));
    assert!(environment_update.contains("<allowed>api.example.com</allowed>"));
    assert!(environment_update.contains("<denied>blocked.example.com</denied>"));
}

#[tokio::test]
async fn build_settings_update_items_emits_environment_item_for_time_changes() {
    let (session, previous_context) = make_session_and_context().await;
    let previous_context = Arc::new(previous_context);
    let mut current_context = previous_context
        .with_model(
            previous_context.model_info.slug.clone(),
            &session.services.models_manager,
        )
        .await;
    current_context.current_date = Some("2026-02-27".to_string());
    current_context.timezone = Some("Europe/Berlin".to_string());

    let reference_context_item = previous_context.to_turn_context_item();
    let update_items = session
        .build_settings_update_items(Some(&reference_context_item), &current_context)
        .await;

    let environment_update = update_items
        .iter()
        .find_map(|item| match item {
            ResponseItem::Message { role, content, .. } if role == "user" => {
                let [ContentItem::InputText { text }] = content.as_slice() else {
                    return None;
                };
                text.contains("<environment_context>").then_some(text)
            }
            _ => None,
        })
        .expect("environment update item should be emitted");
    assert!(environment_update.contains("<current_date>2026-02-27</current_date>"));
    assert!(environment_update.contains("<timezone>Europe/Berlin</timezone>"));
}

#[tokio::test]
async fn build_initial_context_omits_default_image_save_location_with_image_history() {
    let (session, turn_context) = make_session_and_context().await;
    session
        .replace_history(
            vec![ResponseItem::ImageGenerationCall {
                id: "ig-test".to_string(),
                status: "completed".to_string(),
                revised_prompt: Some("a tiny blue square".to_string()),
                result: "Zm9v".to_string(),
            }],
            None,
        )
        .await;

    let initial_context = session.build_initial_context(&turn_context).await;
    let developer_texts = developer_input_texts(&initial_context);
    assert!(
        !developer_texts
            .iter()
            .any(|text| text.contains("Generated images are saved to")),
        "expected initial context to omit image save instructions even with image history, got {developer_texts:?}"
    );
}

#[tokio::test]
async fn build_initial_context_omits_default_image_save_location_without_image_history() {
    let (session, turn_context) = make_session_and_context().await;

    let initial_context = session.build_initial_context(&turn_context).await;
    let developer_texts = developer_input_texts(&initial_context);

    assert!(
        !developer_texts
            .iter()
            .any(|text| text.contains("Generated images are saved to")),
        "expected initial context to omit image save instructions without image history, got {developer_texts:?}"
    );
}

#[tokio::test]
async fn handle_output_item_done_records_image_save_history_message() {
    let (session, turn_context) = make_session_and_context().await;
    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let call_id = "ig_history_records_message";
    let expected_saved_path = std::env::temp_dir().join(format!("{call_id}.png"));
    let _ = std::fs::remove_file(&expected_saved_path);
    let item = ResponseItem::ImageGenerationCall {
        id: call_id.to_string(),
        status: "completed".to_string(),
        revised_prompt: Some("a tiny blue square".to_string()),
        result: "Zm9v".to_string(),
    };

    let mut ctx = HandleOutputCtx {
        sess: Arc::clone(&session),
        turn_context: Arc::clone(&turn_context),
        tool_runtime: test_tool_runtime(Arc::clone(&session), Arc::clone(&turn_context)),
        cancellation_token: CancellationToken::new(),
    };
    handle_output_item_done(&mut ctx, item.clone(), None)
        .await
        .expect("image generation item should succeed");

    let history = session.clone_history().await;
    let save_message: ResponseItem = DeveloperInstructions::new(format!(
        "Generated images are saved to {} as {} by default.",
        std::env::temp_dir().display(),
        std::env::temp_dir().join("<image_id>.png").display(),
    ))
    .into();
    assert_eq!(history.raw_items(), &[save_message, item]);
    assert_eq!(
        std::fs::read(&expected_saved_path).expect("saved file"),
        b"foo"
    );
    let _ = std::fs::remove_file(&expected_saved_path);
}

#[tokio::test]
async fn handle_output_item_done_skips_image_save_message_when_save_fails() {
    let (session, turn_context) = make_session_and_context().await;
    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let call_id = "ig_history_no_message";
    let expected_saved_path = std::env::temp_dir().join(format!("{call_id}.png"));
    let _ = std::fs::remove_file(&expected_saved_path);
    let item = ResponseItem::ImageGenerationCall {
        id: call_id.to_string(),
        status: "completed".to_string(),
        revised_prompt: Some("broken payload".to_string()),
        result: "_-8".to_string(),
    };

    let mut ctx = HandleOutputCtx {
        sess: Arc::clone(&session),
        turn_context: Arc::clone(&turn_context),
        tool_runtime: test_tool_runtime(Arc::clone(&session), Arc::clone(&turn_context)),
        cancellation_token: CancellationToken::new(),
    };
    handle_output_item_done(&mut ctx, item.clone(), None)
        .await
        .expect("image generation item should still complete");

    let history = session.clone_history().await;
    assert_eq!(history.raw_items(), &[item]);
    assert!(!expected_saved_path.exists());
}

#[tokio::test]
async fn record_context_updates_and_set_reference_context_item_injects_full_context_when_baseline_missing()
 {
    let (session, turn_context) = make_session_and_context().await;
    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await;
    let history = session.clone_history().await;
    let initial_context = session.build_initial_context(&turn_context).await;
    assert_eq!(history.raw_items().to_vec(), initial_context);

    let current_context = session.reference_context_item().await;
    assert_eq!(
        serde_json::to_value(current_context).expect("serialize current context item"),
        serde_json::to_value(Some(turn_context.to_turn_context_item()))
            .expect("serialize expected context item")
    );
}

#[tokio::test]
async fn record_context_updates_and_set_reference_context_item_reinjects_full_context_after_clear()
{
    let (session, turn_context) = make_session_and_context().await;
    let compacted_summary = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: format!("{}\nsummary", crate::compact::SUMMARY_PREFIX),
        }],
        end_turn: None,
        phase: None,
    };
    session
        .record_into_history(std::slice::from_ref(&compacted_summary), &turn_context)
        .await;
    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await;
    {
        let mut state = session.state.lock().await;
        state.set_reference_context_item(None);
    }
    session
        .replace_history(vec![compacted_summary.clone()], None)
        .await;

    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await;

    let history = session.clone_history().await;
    let mut expected_history = vec![compacted_summary];
    expected_history.extend(session.build_initial_context(&turn_context).await);
    assert_eq!(history.raw_items().to_vec(), expected_history);
}

#[tokio::test]
async fn record_context_updates_and_set_reference_context_item_persists_baseline_without_emitting_diffs()
 {
    let (session, previous_context) = make_session_and_context().await;
    let next_model = if previous_context.model_info.slug == "gpt-5.1" {
        "gpt-5"
    } else {
        "gpt-5.1"
    };
    let turn_context = previous_context
        .with_model(next_model.to_string(), &session.services.models_manager)
        .await;
    let previous_context_item = previous_context.to_turn_context_item();
    {
        let mut state = session.state.lock().await;
        state.set_reference_context_item(Some(previous_context_item.clone()));
    }
    let config = session.get_config().await;
    let process_id = ProcessId::default();
    let recorder = RolloutRecorder::new(
        config.as_ref(),
        RolloutRecorderParams::new(
            process_id,
            None,
            SessionSource::Exec,
            BaseInstructions::default(),
            Vec::new(),
            EventPersistenceMode::Limited,
        ),
        None,
        None,
    )
    .await
    .expect("create rollout recorder");
    {
        let mut rollout = session.services.rollout.lock().await;
        *rollout = Some(recorder);
    }

    let update_items = session
        .build_settings_update_items(Some(&previous_context_item), &turn_context)
        .await;
    assert_eq!(update_items, Vec::new());

    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await;

    assert_eq!(
        session.clone_history().await.raw_items().to_vec(),
        Vec::new()
    );
    assert_eq!(
        serde_json::to_value(session.reference_context_item().await)
            .expect("serialize current context item"),
        serde_json::to_value(Some(turn_context.to_turn_context_item()))
            .expect("serialize expected context item")
    );
    // Use snapshot_rollout_items to verify the TurnContext was recorded.
    // The in-memory snapshot is the authoritative record of what the
    // recorder accepted; a journal round-trip test belongs in journald's
    // own test suite.
    let snapshot = {
        let guard = session.services.rollout.lock().await;
        guard
            .as_ref()
            .expect("rollout recorder")
            .snapshot_rollout_items()
    };
    let persisted_turn_context = snapshot.iter().find_map(|item| match item {
        RolloutItem::TurnContext(ctx) => Some(ctx.clone()),
        _ => None,
    });
    assert_eq!(
        serde_json::to_value(persisted_turn_context)
            .expect("serialize persisted turn context item"),
        serde_json::to_value(Some(turn_context.to_turn_context_item()))
            .expect("serialize expected turn context item")
    );
}

#[tokio::test]
async fn build_initial_context_prepends_model_switch_message() {
    let (session, turn_context) = make_session_and_context().await;
    let previous_turn_settings = PreviousTurnSettings {
        model: "previous-regular-model".to_string(),
    };

    session
        .set_previous_turn_settings(Some(previous_turn_settings))
        .await;
    let initial_context = session.build_initial_context(&turn_context).await;

    let ResponseItem::Message { role, content, .. } = &initial_context[0] else {
        panic!("expected system message");
    };
    assert_eq!(role, "system");
    let [ContentItem::InputText { text }, ..] = content.as_slice() else {
        panic!("expected system text");
    };
    assert!(text.contains("<model_switch>"));
}

#[tokio::test]
async fn record_context_updates_and_set_reference_context_item_persists_full_reinjection_to_rollout()
 {
    let (session, previous_context) = make_session_and_context().await;
    let next_model = if previous_context.model_info.slug == "gpt-5.1" {
        "gpt-5"
    } else {
        "gpt-5.1"
    };
    let turn_context = previous_context
        .with_model(next_model.to_string(), &session.services.models_manager)
        .await;
    let config = session.get_config().await;
    let process_id = ProcessId::default();
    let recorder = RolloutRecorder::new(
        config.as_ref(),
        RolloutRecorderParams::new(
            process_id,
            None,
            SessionSource::Exec,
            BaseInstructions::default(),
            Vec::new(),
            EventPersistenceMode::Limited,
        ),
        None,
        None,
    )
    .await
    .expect("create rollout recorder");
    {
        let mut rollout = session.services.rollout.lock().await;
        *rollout = Some(recorder);
    }

    session
        .persist_rollout_items(&[RolloutItem::EventMsg(EventMsg::UserMessage(
            UserMessageEvent {
                message: "seed rollout".to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements: Vec::new(),
            },
        ))])
        .await;
    {
        let mut state = session.state.lock().await;
        state.set_reference_context_item(None);
    }

    session
        .set_previous_turn_settings(Some(PreviousTurnSettings {
            model: previous_context.model_info.slug.clone(),
        }))
        .await;
    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await;
    // Use snapshot_rollout_items to verify the TurnContext was recorded.
    let snapshot = {
        let guard = session.services.rollout.lock().await;
        guard
            .as_ref()
            .expect("rollout recorder")
            .snapshot_rollout_items()
    };
    let persisted_turn_context = snapshot.iter().find_map(|item| match item {
        RolloutItem::TurnContext(ctx) => Some(ctx.clone()),
        _ => None,
    });

    assert_eq!(
        serde_json::to_value(persisted_turn_context)
            .expect("serialize persisted turn context item"),
        serde_json::to_value(Some(turn_context.to_turn_context_item()))
            .expect("serialize expected turn context item")
    );
}
