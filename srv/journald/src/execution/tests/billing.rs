use super::*;
use crate::execution::billing::MAX_RECONCILIATIONS_PER_TURN;

pub(super) struct Turn {
    pub(super) scope: String,
    pub(super) lease: ExecutionLease,
    pub(super) turn_id: Uuid,
    pub(super) process_id: ProcessId,
}

impl Turn {
    pub(super) async fn new<S: JournalStore>(store: &S, scope: String) -> Self {
        store
            .execution(ExecutionRequest::BindScope {
                scope: scope.clone(),
                policy_digest: "a".repeat(64),
            })
            .await
            .unwrap();
        let (_, lease) = claimed(
            store
                .execution(ExecutionRequest::ClaimBound {
                    session_id: Uuid::new_v4(),
                    scope: scope.clone(),
                    policy_digest: "a".repeat(64),
                    owner_id: "billing-test".into(),
                    ttl_ms: 300_000,
                })
                .await
                .unwrap(),
        );
        let process_id = ProcessId::new();
        store
            .execution(ExecutionRequest::BindNewProcess {
                lease: lease.clone(),
                process_id,
            })
            .await
            .unwrap();
        let turn_id = Uuid::new_v4();
        store
            .execution(ExecutionRequest::AcceptMetered {
                lease: lease.clone(),
                turn_id,
                fingerprint: "b".repeat(64),
            })
            .await
            .unwrap();
        store
            .execution(ExecutionRequest::Dispatch {
                lease: lease.clone(),
                turn_id,
            })
            .await
            .unwrap();
        Self {
            scope,
            lease,
            turn_id,
            process_id,
        }
    }

    async fn begin<S: JournalStore>(&self, store: &S) -> Uuid {
        let invocation_id = Uuid::new_v4();
        store
            .execution(ExecutionRequest::Invocation {
                request: ExecutionInvocationRequest::Begin {
                    lease: self.lease.clone(),
                    request: InvocationStart {
                        invocation_id,
                        turn_id: self.turn_id,
                        process_id: self.process_id,
                        provider_id: "provider".into(),
                        requested_model: "requested-not-attested".into(),
                        requested_service_tier: None,
                        purpose: InvocationPurpose::Sampling,
                    },
                },
            })
            .await
            .unwrap();
        invocation_id
    }

    fn reconcile(&self, record: ExecutionReconciliation) -> ExecutionRequest {
        ExecutionRequest::Billing {
            request: ExecutionBillingRequest::Reconcile {
                scope: self.scope.clone(),
                session_id: self.lease.session_id,
                turn_id: self.turn_id,
                record,
            },
        }
    }

    async fn report<S: JournalStore>(&self, store: &S) -> ExecutionBillingReport {
        let ExecutionResponse::Billing(ExecutionBillingResponse::Report(report)) = store
            .execution(ExecutionRequest::Billing {
                request: ExecutionBillingRequest::Read {
                    scope: self.scope.clone(),
                    session_id: self.lease.session_id,
                    turn_id: self.turn_id,
                },
            })
            .await
            .unwrap()
        else {
            panic!("expected billing report")
        };
        assert!(!report.billing_complete);
        report
    }

    pub(super) async fn accounting<S: JournalStore>(&self, store: &S) -> ExecutionAccounting {
        let ExecutionResponse::Accounting(accounting) = store
            .execution(ExecutionRequest::Invocation {
                request: ExecutionInvocationRequest::Read {
                    session_id: self.lease.session_id,
                    turn_id: self.turn_id,
                },
            })
            .await
            .unwrap()
        else {
            panic!("expected native accounting")
        };
        accounting
    }
}

fn prices() -> ExecutionPriceSchedule {
    ExecutionPriceSchedule {
        version: "published-v1".into(),
        provider_id: "provider".into(),
        model: "attested-model".into(),
        service_tier: "attested-tier".into(),
        currency: "USD".into(),
        tokens_per_unit: 100,
        prices: ExecutionTokenPrices {
            uncached_input: 2,
            cache_creation_input: 3,
            cached_input: 5,
            ordinary_output: 7,
            reasoning_output: 11,
        },
        source_sha256: "c".repeat(64),
        actor: "operator".into(),
        reason: "verified published provider schedule".into(),
    }
}

fn usage() -> InvocationTokenUsage {
    InvocationTokenUsage {
        input_tokens: 3,
        cache_creation_input_tokens: 1,
        cached_input_tokens: 1,
        output_tokens: 2,
        reasoning_output_tokens: 1,
        // Provider totals are evidence, not a sixth priced bucket.
        total_tokens: 999,
    }
}

fn evidence(invocation_id: Uuid) -> ExecutionReconciliation {
    ExecutionReconciliation {
        reconciliation_id: Uuid::new_v4(),
        invocation_id,
        provider_receipt_id: Uuid::new_v4().to_string(),
        evidence_sha256: "d".repeat(64),
        model: "attested-model".into(),
        service_tier: "attested-tier".into(),
        usage: Some(usage()),
        charged: None,
        price_version: Some("published-v1".into()),
        actor: "operator".into(),
        reason: "verified external attempt receipt".into(),
    }
}

async fn put_price<S: JournalStore>(
    store: &S,
    scope: &str,
    schedule: ExecutionPriceSchedule,
) -> Result<ExecutionResponse, JournalError> {
    store
        .execution(ExecutionRequest::Billing {
            request: ExecutionBillingRequest::PutPrice {
                scope: scope.into(),
                schedule,
            },
        })
        .await
}

async fn assert_price_evidence<S: JournalStore>(store: &S) {
    let turn = Turn::new(store, Uuid::new_v4().to_string()).await;
    let invocation_id = turn.begin(store).await;
    let schedule = prices();
    // 2+3+5+7+11 = 28; ceil(28/100)=1, not five separately rounded units.
    assert_eq!(schedule.estimate(&usage()).unwrap().nanounits, "1");
    let mut exact = schedule.clone();
    exact.tokens_per_unit = 1;
    assert_eq!(exact.estimate(&usage()).unwrap().nanounits, "28");
    exact.tokens_per_unit = 10;
    assert_eq!(exact.estimate(&usage()).unwrap().nanounits, "3");
    let mut large = schedule.clone();
    large.tokens_per_unit = 1;
    large.prices.uncached_input = u64::MAX;
    let amount = large
        .estimate(&InvocationTokenUsage {
            input_tokens: i64::MAX,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        amount.nanounits,
        (i64::MAX as u128 * u128::from(u64::MAX)).to_string()
    );
    let original = put_price(store, &turn.scope, schedule.clone())
        .await
        .unwrap();
    assert_eq!(
        put_price(store, &turn.scope, schedule.clone())
            .await
            .unwrap(),
        original
    );
    let mut altered = schedule.clone();
    altered.prices.cached_input += 1;
    assert!(put_price(store, &turn.scope, altered).await.is_err());
    assert_eq!(
        store
            .execution(ExecutionRequest::Billing {
                request: ExecutionBillingRequest::GetPrice {
                    scope: turn.scope.clone(),
                    version: schedule.version.clone(),
                },
            })
            .await
            .unwrap(),
        original
    );
    for mismatch in 0..4 {
        let mut record = evidence(invocation_id);
        let mut mismatched_price = schedule.clone();
        mismatched_price.version = format!("mismatch-{mismatch}");
        match mismatch {
            0 => mismatched_price.provider_id = "other-provider".into(),
            1 => record.model = "requested-not-attested".into(),
            2 => record.service_tier = "requested-not-attested".into(),
            _ => {
                record.charged = Some(ExecutionMoney {
                    currency: "EUR".into(),
                    nanounits: "1".into(),
                })
            }
        }
        record.price_version = Some(mismatched_price.version.clone());
        put_price(store, &turn.scope, mismatched_price)
            .await
            .unwrap();
        assert!(store.execution(turn.reconcile(record)).await.is_err());
    }
    for invalid in 0..6 {
        let mut record = evidence(invocation_id);
        match invalid {
            0 => record.usage = None,
            1 => record.usage.as_mut().unwrap().input_tokens = -1,
            2 => record.usage.as_mut().unwrap().cached_input_tokens = 4,
            3 => record.usage.as_mut().unwrap().reasoning_output_tokens = 3,
            4 => {
                record.charged = Some(ExecutionMoney {
                    currency: "USD".into(),
                    nanounits: "-1".into(),
                })
            }
            _ => {
                record.charged = Some(ExecutionMoney {
                    currency: "USD".into(),
                    nanounits: "01".into(),
                })
            }
        }
        assert!(store.execution(turn.reconcile(record)).await.is_err());
    }
    assert!(turn.report(store).await.records.is_empty());
    let request = turn.reconcile(evidence(invocation_id));
    let first = store.execution(request.clone()).await.unwrap();
    assert_eq!(store.execution(request).await.unwrap(), first);
    let report = turn.report(store).await;
    assert_eq!(report.records.len(), 1);
    assert_eq!(
        report.records[0].estimated,
        Some(ExecutionMoney {
            currency: "USD".into(),
            nanounits: "1".into()
        })
    );
}

async fn assert_receipt_fencing<S: JournalStore>(store: &S) {
    let first = Turn::new(store, Uuid::new_v4().to_string()).await;
    let second = Turn::new(store, first.scope.clone()).await;
    let a = first.begin(store).await;
    let b = second.begin(store).await;
    let mut left = evidence(a);
    left.price_version = None;
    let mut right = left.clone();
    right.reconciliation_id = Uuid::new_v4();
    right.invocation_id = b;
    let (one, two) = tokio::join!(
        store.execution(first.reconcile(left.clone())),
        store.execution(second.reconcile(right.clone()))
    );
    assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
    let (winner, record, saved, loser) = if let Ok(saved) = one {
        (&first, left, saved, &second)
    } else {
        (&second, right, two.unwrap(), &first)
    };
    assert_eq!(
        store
            .execution(winner.reconcile(record.clone()))
            .await
            .unwrap(),
        saved,
        "committed identity retry must retain the original timestamp"
    );
    let mut changed = record.clone();
    changed.reason.push_str(" changed");
    assert!(store.execution(winner.reconcile(changed)).await.is_err());
    assert!(
        store
            .execution(loser.reconcile(record.clone()))
            .await
            .is_err(),
        "another session cannot adopt an existing reconciliation UUID"
    );
    let foreign = Turn::new(store, Uuid::new_v4().to_string()).await;
    let foreign_invocation = foreign.begin(store).await;
    for scope in [&foreign.scope, "nonexistent-scope"] {
        assert!(
            store
                .execution(ExecutionRequest::Billing {
                    request: ExecutionBillingRequest::Read {
                        scope: scope.into(),
                        session_id: winner.lease.session_id,
                        turn_id: winner.turn_id,
                    }
                })
                .await
                .is_err()
        );
    }
    assert!(
        store
            .execution(foreign.reconcile(record.clone()))
            .await
            .is_err()
    );
    let mut steal = record.clone();
    steal.reconciliation_id = Uuid::new_v4();
    assert!(
        store
            .execution(foreign.reconcile(steal.clone()))
            .await
            .is_err(),
        "foreign invocation cannot be attached to a locally owned turn"
    );
    // Receipt IDs are provider+scope scoped, not globally visible across tenants.
    steal.invocation_id = foreign_invocation;
    store.execution(foreign.reconcile(steal)).await.unwrap();
    assert_eq!(winner.report(store).await.records.len(), 1);
    assert!(loser.report(store).await.records.is_empty());
    assert_eq!(foreign.report(store).await.records.len(), 1);
}

async fn assert_native_receipts_and_limit<S: JournalStore>(store: &S) {
    let turn = Turn::new(store, Uuid::new_v4().to_string()).await;
    let completed = turn.begin(store).await;
    let unknown = turn.begin(store).await;
    for (invocation_id, receipt) in [
        (
            completed,
            InvocationReceipt {
                completed: true,
                response_id: Some("native-response".into()),
                server_model: Some("native-observed-model".into()),
                usage: Some(usage()),
            },
        ),
        (unknown, InvocationReceipt::unknown()),
    ] {
        store
            .execution(ExecutionRequest::Invocation {
                request: ExecutionInvocationRequest::Complete {
                    lease: turn.lease.clone(),
                    invocation_id,
                    receipt,
                },
            })
            .await
            .unwrap();
    }
    let before = turn.accounting(store).await;
    let mut initial = evidence(unknown);
    initial.usage = None;
    initial.price_version = None;
    initial.charged = Some(ExecutionMoney {
        currency: "USD".into(),
        nanounits: "71".into(),
    });
    let saved = store
        .execution(turn.reconcile(initial.clone()))
        .await
        .unwrap();
    for invocation_id in [completed, unknown] {
        assert!(
            store
                .execution(ExecutionRequest::Invocation {
                    request: ExecutionInvocationRequest::Complete {
                        lease: turn.lease.clone(),
                        invocation_id,
                        receipt: InvocationReceipt {
                            completed: true,
                            response_id: Some("external-evidence-is-not-native".into()),
                            server_model: Some("attested-model".into()),
                            usage: Some(usage()),
                        },
                    },
                })
                .await
                .is_err(),
            "external evidence must not authorize replacing a committed native receipt"
        );
    }
    // More than one hidden wire attempt may belong to one logical invocation.
    for index in 1..MAX_RECONCILIATIONS_PER_TURN {
        let mut record = evidence(if index % 2 == 0 { completed } else { unknown });
        record.price_version = None;
        store.execution(turn.reconcile(record)).await.unwrap();
    }
    assert_eq!(turn.accounting(store).await, before);
    let report = turn.report(store).await;
    assert_eq!(report.records.len(), MAX_RECONCILIATIONS_PER_TURN as usize);
    assert!(
        report
            .records
            .iter()
            .all(|record| record.estimated.is_none())
    );
    let mut overflow = evidence(unknown);
    overflow.price_version = None;
    assert!(store.execution(turn.reconcile(overflow)).await.is_err());
    assert_eq!(
        store.execution(turn.reconcile(initial)).await.unwrap(),
        saved,
        "capacity must not reject an already committed identity retry"
    );
    assert_eq!(turn.accounting(store).await, before);
}

macro_rules! postgres_case {
    ($postgres:ident, $assertion:ident) => {
        #[tokio::test]
        async fn $postgres() {
            let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
                eprintln!("skipping PostgreSQL billing validation; TEST_DATABASE_URL is not set");
                return;
            };
            let pool = chaos_proc::open_runtime_db_postgres_url(&url)
                .await
                .unwrap();
            let store = crate::PostgresJournalStore::new(pool);
            $assertion(&store).await;
        }
    };
}

postgres_case!(
    postgres_exact_prices_reject_unattested_or_invalid_evidence,
    assert_price_evidence
);
postgres_case!(
    postgres_external_receipts_have_one_scoped_owner,
    assert_receipt_fencing
);
postgres_case!(
    postgres_reconciliation_never_rewrites_native_receipts_or_bypasses_capacity,
    assert_native_receipts_and_limit
);
