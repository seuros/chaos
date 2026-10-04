use super::*;

fn observation(thermal: chaos_machine::ThermalState) -> MachineStatus {
    use chaos_machine::*;
    let machine = MachineSnapshot {
        observed_at: SystemTime::now(),
        os: "test",
        arch: "test",
        profile: MachineProfile::default(),
        power: PowerInfo::default(),
        thermal: ThermalInfo {
            state: thermal,
            cpu_temperatures: vec![],
        },
    };
    MachineStatus {
        machine,
        storage: StorageSnapshot {
            observed_at: SystemTime::now(),
            filesystems: vec![],
            unavailable: vec![],
        },
        warnings: if thermal == ThermalState::Critical {
            vec![crate::machine_warnings::MachineWarning::Thermal { state: thermal }]
        } else {
            vec![]
        },
        warning_instruction: None,
    }
}

async fn sample(session: &Session, thermal: chaos_machine::ThermalState) {
    session
        .refresh_machine_recovery_with_observer(|_| async move { Ok(observation(thermal)) })
        .await
        .unwrap();
}

async fn unresolved(session: &Session) {
    sample(session, chaos_machine::ThermalState::Critical).await;
}

#[tokio::test]
async fn machine_recovery_wait_parks_only_after_a_single_tool_batch() {
    let (session, turn) = crate::chaos::make_session_and_context().await;
    unresolved(&session).await;
    let id = session
        .state
        .lock()
        .await
        .machine_recovery
        .request_wait(&turn.sub_id)
        .unwrap();
    {
        let mut state = session.state.lock().await;
        let recovery = &mut state.machine_recovery;
        assert!(recovery.park_wait("another turn").is_none());
        assert_eq!(recovery.wait_id(), Some(id.as_str()));
        assert_eq!(recovery.requested_turn(), Some(turn.sub_id.as_str()));
    }
    assert!(!session.machine_recovery_parked().await);
    session.finish_machine_wait_request(&turn, 2).await;
    assert!(!session.machine_recovery_parked().await);
    assert!(session.machine_recovery_status().await.wait_id.is_none());
    assert_eq!(session.clone_history().await.raw_items().len(), 1);
    session.state.lock().await.machine_recovery.begin_sample();
    session
        .state
        .lock()
        .await
        .machine_recovery
        .request_wait(&turn.sub_id)
        .unwrap();
    session.finish_machine_wait_request(&turn, 1).await;
    assert!(session.machine_recovery_parked().await);
    assert!(
        session
            .state
            .lock()
            .await
            .machine_recovery
            .requested_turn()
            .is_none()
    );
    session.cancel_machine_recovery_wait().await;
    assert!(!session.machine_recovery_parked().await);
    session.finish_machine_wait_request(&turn, 1).await;
    assert!(session.machine_recovery_status().await.wait_id.is_none());
}

#[tokio::test]
async fn machine_recovery_cancellation_revokes_an_already_queued_success() {
    let (session, turn) = crate::chaos::make_session_and_context().await;
    unresolved(&session).await;
    let id = session
        .state
        .lock()
        .await
        .machine_recovery
        .request_wait(&turn.sub_id)
        .unwrap();
    session.finish_machine_wait_request(&turn, 1).await;
    session
        .services
        .internal_task_store
        .register(BackgroundTask {
            id: id.clone(),
            source: Some(TaskSource::MachineRecovery),
            state: TaskState::Succeeded,
            status_message: None,
            created_at: "now".into(),
            updated_at: "now".into(),
            result: None,
            origin_call_id: None,
            origin_turn_id: None,
            execution_id: None,
            ready: true,
            notify: true,
            delivered: false,
        })
        .await;
    assert_eq!(
        session.services.internal_task_store.pending().await.len(),
        1
    );
    session.cancel_machine_recovery_wait().await;
    assert!(
        session
            .services
            .internal_task_store
            .pending()
            .await
            .is_empty()
    );
    assert_eq!(
        session
            .services
            .internal_task_store
            .get(&id)
            .await
            .unwrap()
            .state,
        TaskState::Cancelled
    );
}

#[tokio::test]
async fn machine_recovery_monitor_only_completes_opted_in_waits_once() {
    use chaos_machine::ThermalState;
    for opted_in in [false, true] {
        let (session, turn) = crate::chaos::make_session_and_context().await;
        tokio::time::pause();
        unresolved(&session).await;
        if opted_in {
            session
                .state
                .lock()
                .await
                .machine_recovery
                .request_wait(&turn.sub_id)
                .unwrap();
            session.finish_machine_wait_request(&turn, 1).await;
            assert_eq!(
                session
                    .services
                    .internal_task_store
                    .subscribe()
                    .borrow()
                    .outstanding_tasks,
                1
            );
        }
        sample(&session, ThermalState::Normal).await;
        for _ in 0..9 {
            tokio::time::advance(POLL_INTERVAL).await;
            sample(&session, ThermalState::Normal).await;
            assert!(
                session
                    .services
                    .internal_task_store
                    .pending()
                    .await
                    .is_empty()
            );
        }
        tokio::time::advance(POLL_INTERVAL).await;
        sample(&session, ThermalState::Normal).await;
        assert_eq!(
            session.machine_recovery_status().await.phase,
            Phase::Recovered
        );
        let pending = session.services.internal_task_store.pending().await;
        assert_eq!(pending.len(), usize::from(opted_in));
        if opted_in {
            let id = pending[0].id.clone();
            sample(&session, ThermalState::Critical).await;
            assert!(
                session
                    .services
                    .internal_task_store
                    .pending()
                    .await
                    .is_empty()
            );
            assert_eq!(
                session
                    .services
                    .internal_task_store
                    .get(&id)
                    .await
                    .unwrap()
                    .state,
                TaskState::Running
            );
            sample(&session, ThermalState::Normal).await;
            for _ in 0..10 {
                tokio::time::advance(POLL_INTERVAL).await;
                sample(&session, ThermalState::Normal).await;
            }
            assert_eq!(
                session.services.internal_task_store.pending().await.len(),
                1
            );
            session
                .services
                .internal_task_store
                .acknowledge(&[id], "wake")
                .await;
            sample(&session, ThermalState::Normal).await;
            assert!(
                session
                    .services
                    .internal_task_store
                    .pending()
                    .await
                    .is_empty()
            );
            assert_eq!(session.services.internal_task_store.list().await.len(), 1);
        }
        tokio::time::resume();
    }
}

#[tokio::test]
async fn machine_recovery_probe_timeout_resets_the_window() {
    let (session, _) = crate::chaos::make_session_and_context().await;
    tokio::time::pause();
    unresolved(&session).await;
    sample(&session, chaos_machine::ThermalState::Normal).await;
    let result = session
        .refresh_machine_recovery_with_observer(|_| std::future::pending())
        .await;
    assert!(result.is_err());
    let status = session.machine_recovery_status().await;
    assert_eq!(status.phase, Phase::Unavailable);
    assert_eq!(status.stable_seconds, 0);
}

#[tokio::test]
async fn machine_recovery_parked_session_does_not_admit_unrelated_completions() {
    let (session, turn) = crate::chaos::make_session_and_context().await;
    let session = Arc::new(session);
    unresolved(&session).await;
    session
        .state
        .lock()
        .await
        .machine_recovery
        .request_wait(&turn.sub_id)
        .unwrap();
    session.finish_machine_wait_request(&turn, 1).await;
    session
        .services
        .internal_task_store
        .register(BackgroundTask {
            id: "other-work".into(),
            source: None,
            state: TaskState::Succeeded,
            status_message: None,
            created_at: "now".into(),
            updated_at: "now".into(),
            result: None,
            origin_call_id: None,
            origin_turn_id: None,
            execution_id: None,
            ready: true,
            notify: true,
            delivered: false,
        })
        .await;
    session.admit_completion_turn().await;
    assert!(session.active_turn.lock().await.is_none());
    assert!(session.machine_recovery_parked().await);
    assert_eq!(
        session.services.internal_task_store.pending().await.len(),
        1
    );
}
