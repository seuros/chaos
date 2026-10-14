use super::*;
use chaos_journald::{
    ExecutionAccounting, ExecutionInvocationRequest as InvocationRequest, InvocationPurpose,
    InvocationReceipt, InvocationStart, InvocationState, InvocationTokenUsage,
};

pub(super) fn start(
    fixture: &Fixture,
    turn_id: Uuid,
    purpose: InvocationPurpose,
) -> InvocationStart {
    InvocationStart {
        invocation_id: Uuid::new_v4(),
        turn_id,
        process_id: ProcessId::from_string(&fixture.session.snapshot().process_id.unwrap())
            .unwrap(),
        provider_id: "test-provider".into(),
        requested_model: "requested-model".into(),
        requested_service_tier: None,
        purpose,
    }
}

fn receipt(id: &str, usage: Option<InvocationTokenUsage>) -> InvocationReceipt {
    InvocationReceipt {
        completed: true,
        response_id: Some(id.into()),
        server_model: Some("served-model".into()),
        usage,
    }
}

async fn accounting(fixture: &Fixture, id: Uuid) -> ExecutionAccounting {
    fixture
        .session
        .ledger
        .get()
        .unwrap()
        .accounting(id)
        .await
        .unwrap()
}

async fn running(fixture: &Fixture) -> Uuid {
    let id = Uuid::new_v4();
    fixture.admit(&id.to_string()).await.unwrap();
    fixture
        .session
        .ledger
        .get()
        .unwrap()
        .dispatch(&id.to_string())
        .await
        .unwrap();
    id
}

async fn current_lease(fixture: &Fixture) -> chaos_journald::ExecutionLease {
    let ExecutionResponse::Claimed { lease, .. } = fixture
        .store
        .call(ExecutionRequest::ClaimBound {
            session_id: Uuid::parse_str(&fixture.session.id).unwrap(),
            scope: fixture.store.scope.clone(),
            policy_digest: fixture.store.policy_digest.clone().unwrap(),
            owner_id: fixture.manager.daemon_id.clone(),
            ttl_ms: 90_000,
        })
        .await
        .unwrap()
    else {
        panic!("expected lease")
    };
    lease
}

#[tokio::test]
async fn receipts_survive_takeover_and_compaction_without_cumulative_delta_billing() {
    let mut fixture = Fixture::new().await;
    let id = running(&fixture).await;
    let sink = fixture.session.ledger.get().unwrap().invocation_sink();
    let normal = start(&fixture, id, InvocationPurpose::Sampling);
    let compact = start(&fixture, id, InvocationPurpose::Compaction);
    let missing = start(&fixture, id, InvocationPurpose::Sampling);
    let pending = start(&fixture, id, InvocationPurpose::Sampling);
    for request in [&normal, &compact, &missing, &pending] {
        assert!(sink.begin(request.clone()).await.unwrap());
    }
    let usage = InvocationTokenUsage {
        input_tokens: 41,
        cache_creation_input_tokens: 13,
        cached_input_tokens: 17,
        output_tokens: 19,
        reasoning_output_tokens: 3,
        total_tokens: 60,
    };
    let reported = receipt("sampling-response", Some(usage.clone()));
    sink.complete(normal.invocation_id, reported.clone())
        .await
        .unwrap();
    sink.complete(normal.invocation_id, reported).await.unwrap();
    sink.complete(
        compact.invocation_id,
        receipt("compaction-response", Some(usage)),
    )
    .await
    .unwrap();
    sink.complete(
        missing.invocation_id,
        receipt("missing-usage-response", None),
    )
    .await
    .unwrap();
    // A context recompute/reset snapshot is not a receipt and cannot become billing.
    let cumulative = chaos_ipc::protocol::TokenUsage {
        input_tokens: 9_999_999,
        total_tokens: 1,
        ..Default::default()
    };
    fixture.session.observe(&Event {
        id: id.to_string(),
        msg: EventMsg::TokenCount(chaos_ipc::protocol::TokenCountEvent {
            info: Some(chaos_ipc::protocol::TokenUsageInfo {
                total_token_usage: cumulative.clone(),
                last_token_usage: cumulative,
                model_context_window: None,
            }),
            rate_limits: None,
            provider_request_started: false,
        }),
    });
    let before = accounting(&fixture, id).await;
    let summary = before.summary.as_ref().unwrap();
    assert!(!summary.billing_complete);
    assert_eq!(summary.reported_invocations, 2);
    assert_eq!(summary.completed_without_usage, 1);
    assert_eq!(summary.pending_invocations, 1);
    let known = summary.known_usage.as_ref().unwrap();
    assert_eq!(
        (
            known.input_tokens,
            known.cache_creation_input_tokens,
            known.cached_input_tokens
        ),
        (82, 26, 34)
    );
    assert_eq!(
        (known.output_tokens, known.reasoning_output_tokens),
        (38, 6)
    );

    drop(fixture.commands.take());
    fixture.db.expire(&fixture.session.id).await;
    let (_manager, recovered, capacity) = fixture.restored().await;
    assert_eq!(capacity.available_permits(), 0);
    assert!(
        sink.complete(pending.invocation_id, receipt("late-receipt", None))
            .await
            .is_err()
    );
    let after = accounting(&fixture, id).await;
    assert_eq!(
        after.summary.as_ref().unwrap().known_usage,
        summary.known_usage
    );
    assert_eq!(after.summary.as_ref().unwrap().unknown_invocations, 1);
    assert_eq!(after.summary.as_ref().unwrap().pending_invocations, 0);
    assert_eq!(
        after
            .invocations
            .iter()
            .filter(|i| i.state == InvocationState::Completed)
            .count(),
        3
    );
    let view = serde_json::to_value(recovered.recovery_view().await.unwrap()).unwrap();
    recovered.resolve(serde_json::from_value(serde_json::json!({
        "resolution_id": Uuid::new_v4(), "expected_process_id": recovered.snapshot().process_id,
        "expected_turn_id": id, "expected_recovery_epoch": view["session"]["recovery_epoch"],
        "containment_confirmed": true, "actor": "operator", "reason": "no provider executor remains",
    })).unwrap()).await.unwrap();
    assert_eq!(
        accounting(&fixture, id).await,
        after,
        "recovery cannot manufacture or erase receipts"
    );
    let configured = "d".repeat(64);
    fixture
        .store
        .call(ExecutionRequest::Delivery {
            request: chaos_journald::ExecutionDeliveryRequest::Configure {
                scope: fixture.store.scope.clone(),
                destination_digest: configured.clone(),
            },
        })
        .await
        .unwrap();
    let ExecutionResponse::Delivery(Some(delivery)) = fixture
        .store
        .call(ExecutionRequest::Delivery {
            request: chaos_journald::ExecutionDeliveryRequest::Claim {
                scope: fixture.store.scope.clone(),
                destination_digest: configured,
                owner_id: "inspect".into(),
                ttl_ms: 60_000,
            },
        })
        .await
        .unwrap()
    else {
        panic!("expected terminal event")
    };
    let event: chaos_journald::ExecutionEvent = serde_json::from_str(&delivery.body_json).unwrap();
    assert_eq!(event.accounting, after.summary);
    assert_eq!(capacity.available_permits(), 1);
    fixture.db.close().await;
}

#[tokio::test]
async fn failed_begin_or_receipt_is_not_free_usage_and_failed_writers_cannot_resume() {
    for completion in [false, true] {
        let mut fixture = Fixture::new().await;
        let id = running(&fixture).await;
        let sink = fixture.session.ledger.get().unwrap().invocation_sink();
        let request = start(&fixture, id, InvocationPurpose::Sampling);
        if completion {
            assert!(sink.begin(request.clone()).await.unwrap());
        }
        fixture.db.fail_invocation(completion).await;
        if completion {
            assert!(
                sink.complete(
                    request.invocation_id,
                    receipt(
                        "lost-receipt",
                        Some(InvocationTokenUsage {
                            input_tokens: 10,
                            output_tokens: 5,
                            total_tokens: 15,
                            ..Default::default()
                        })
                    )
                )
                .await
                .is_err()
            );
        } else {
            assert!(sink.begin(request.clone()).await.is_err());
        }
        let saved = accounting(&fixture, id).await;
        assert_eq!(saved.invocations.len(), usize::from(completion));
        if completion {
            assert_eq!(saved.invocations[0].state, InvocationState::Started);
            assert!(saved.invocations[0].receipt.is_none());
        }
        assert_eq!(fixture.capacity.available_permits(), 0);
        fixture.db.clear_invocation_fault().await;
        assert!(
            sink.begin(request).await.is_err(),
            "ambiguous writer cannot authorize another dispatch"
        );
        drop(fixture.commands.take());
        fixture.db.expire(&fixture.session.id).await;
        let (_manager, _recovered, capacity) = fixture.restored().await;
        let saved = accounting(&fixture, id).await;
        assert_eq!(
            saved.summary.as_ref().unwrap().unknown_invocations,
            u64::from(completion)
        );
        assert_eq!(capacity.available_permits(), 0);
        fixture.db.close().await;
    }
}

#[tokio::test]
async fn duplicate_provider_receipts_and_conflicting_invocation_payloads_cannot_double_count() {
    let fixture = Fixture::new().await;
    let id = running(&fixture).await;
    let sink = fixture.session.ledger.get().unwrap().invocation_sink();
    let first = start(&fixture, id, InvocationPurpose::Sampling);
    let second = start(&fixture, id, InvocationPurpose::Sampling);
    assert!(sink.begin(first.clone()).await.unwrap());
    assert!(
        !sink.begin(first.clone()).await.unwrap(),
        "start retry is read-only, never dispatch permission"
    );
    let mut changed = first.clone();
    changed.requested_model = "changed-model".into();
    assert!(
        fixture
            .store
            .call(ExecutionRequest::Invocation {
                request: InvocationRequest::Begin {
                    lease: current_lease(&fixture).await,
                    request: changed
                },
            })
            .await
            .is_err(),
        "an invocation UUID cannot change its execution metadata"
    );
    assert!(sink.begin(second.clone()).await.unwrap());
    let reported = receipt(
        "same-provider-response",
        Some(InvocationTokenUsage {
            input_tokens: 20,
            output_tokens: 10,
            total_tokens: 30,
            ..Default::default()
        }),
    );
    sink.complete(first.invocation_id, reported.clone())
        .await
        .unwrap();
    sink.complete(second.invocation_id, reported).await.unwrap();
    let saved = accounting(&fixture, id).await;
    assert_eq!(
        saved
            .summary
            .as_ref()
            .unwrap()
            .known_usage
            .as_ref()
            .unwrap()
            .total_tokens,
        30
    );
    assert_eq!(saved.summary.as_ref().unwrap().duplicate_receipts, 1);
    assert_eq!(
        saved
            .invocations
            .iter()
            .find(|i| i.request.invocation_id == second.invocation_id)
            .unwrap()
            .duplicate_receipt_of,
        Some(first.invocation_id)
    );
    for response_id in ["overflow-one", "overflow-two"] {
        let request = start(&fixture, id, InvocationPurpose::Sampling);
        assert!(sink.begin(request.clone()).await.unwrap());
        sink.complete(
            request.invocation_id,
            receipt(
                response_id,
                Some(InvocationTokenUsage {
                    input_tokens: i64::MAX,
                    total_tokens: i64::MAX,
                    ..Default::default()
                }),
            ),
        )
        .await
        .unwrap();
    }
    let saved = accounting(&fixture, id).await;
    assert!(saved.summary.as_ref().unwrap().totals_overflowed);
    assert!(
        saved.summary.as_ref().unwrap().known_usage.is_none(),
        "overflow is not saturation or a billable zero"
    );
    assert!(
        sink.complete(first.invocation_id, receipt("replacement-response", None))
            .await
            .is_err()
    );
    assert_eq!(accounting(&fixture, id).await, saved);
    fixture.db.close().await;
}

#[tokio::test]
async fn invocation_limit_stops_before_dispatch_and_legacy_absence_is_not_zero() {
    let fixture = Fixture::new().await;
    let owner = fixture.session.ledger.get().unwrap();
    let lease = current_lease(&fixture).await;
    let legacy = Uuid::new_v4();
    fixture
        .store
        .call(ExecutionRequest::Accept {
            lease: lease.clone(),
            turn_id: legacy,
            fingerprint: "a".repeat(64),
        })
        .await
        .unwrap();
    assert_eq!(owner.accounting(legacy).await.unwrap().summary, None);
    fixture
        .store
        .call(ExecutionRequest::Finish {
            lease,
            turn_id: legacy,
            state: ExecutionTurnState::Interrupted,
            outcome: None,
        })
        .await
        .unwrap();
    let id = running(&fixture).await;
    let sink = owner.invocation_sink();
    for _ in 0..chaos_journald::MAX_EXECUTION_INVOCATIONS {
        assert!(
            sink.begin(start(&fixture, id, InvocationPurpose::Sampling))
                .await
                .unwrap()
        );
    }
    assert!(
        sink.begin(start(&fixture, id, InvocationPurpose::Sampling))
            .await
            .is_err()
    );
    let saved = accounting(&fixture, id).await;
    assert_eq!(
        saved.invocations.len(),
        chaos_journald::MAX_EXECUTION_INVOCATIONS as usize
    );
    assert_eq!(
        saved.summary.as_ref().unwrap().pending_invocations,
        saved.invocations.len() as u64
    );
    assert_eq!(fixture.capacity.available_permits(), 0);
    fixture.db.close().await;
}
