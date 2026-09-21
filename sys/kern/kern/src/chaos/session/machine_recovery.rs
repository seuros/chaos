//! Observation producers never spawn model turns; the runner owns wake admission.
use std::sync::Arc;
use std::time::SystemTime;

use chaos_ipc::background_tasks::{BackgroundTask, TaskSource, TaskState};
use chaos_ipc::models::DeveloperInstructions;
use chaos_ipc::protocol::{Event, EventMsg, WarningEvent};
use tokio::time::Instant;

use super::Session;
use crate::chaos::TurnContext;
use crate::machine_recovery::{POLL_INTERVAL, Phase, Recovery, RecoveryStatus};
use crate::machine_status::{MachineStatus, ObservationRequest};

impl Session {
    pub(crate) fn start_machine_recovery_monitor(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let cancel = self.services.internal_task_store.observer_cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = async {
                        if let Some(session) = weak.upgrade() {
                            if session.get_config().await.machine_warnings.enabled {
                                let _ = session.refresh_machine_recovery().await;
                            } else {
                                session.cancel_machine_recovery_wait().await;
                            }
                        }
                    } => {}
                }
                if weak.strong_count() == 0 {
                    break;
                }
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(POLL_INTERVAL) => {}
                }
            }
        });
    }

    pub(crate) async fn cancel_machine_recovery_wait(&self) {
        let id = self.state.lock().await.machine_recovery.cancel_wait();
        if let Some(id) = id {
            self.services
                .internal_task_store
                .cancel_machine_wake(&id)
                .await;
            self.send_event_raw(Event {
                id: "machine-recovery".into(),
                msg: EventMsg::Warning(WarningEvent {
                    message: "Machine recovery wait cancelled; no recovery wake will be scheduled."
                        .into(),
                }),
            })
            .await;
        }
    }

    pub(crate) async fn machine_recovery_parked(&self) -> bool {
        self.state.lock().await.machine_recovery.parked
    }

    pub(crate) async fn discard_machine_wait_request(&self, turn: &TurnContext) {
        let requested = self
            .state
            .lock()
            .await
            .machine_recovery
            .requested_turn
            .as_deref()
            == Some(&turn.sub_id);
        if requested {
            self.cancel_machine_recovery_wait().await;
        }
    }

    pub(crate) async fn machine_recovery_status(&self) -> RecoveryStatus {
        let config = self.get_config().await;
        self.state
            .lock()
            .await
            .machine_recovery
            .status(&config.machine_warnings, Instant::now())
    }

    pub(crate) async fn machine_recovery_instruction(&self) -> Option<String> {
        let config = self.get_config().await;
        self.state
            .lock()
            .await
            .machine_recovery
            .instruction(&config.machine_warnings, Instant::now())
    }

    pub(crate) async fn refresh_machine_recovery(&self) -> Result<MachineStatus, String> {
        self.refresh_machine_recovery_with_observer(
            |request| async move { request.observe().await },
        )
        .await
    }

    async fn refresh_machine_recovery_with_observer<F, Fut>(
        &self,
        observe: F,
    ) -> Result<MachineStatus, String>
    where
        F: FnOnce(ObservationRequest) -> Fut,
        Fut: Future<Output = Result<MachineStatus, String>>,
    {
        let config = self.get_config().await;
        match tokio::time::timeout(
            std::time::Duration::from_millis(config.machine_warnings.probe_timeout_ms),
            self.observe_machine_recovery(observe),
        )
        .await
        {
            Ok(observation) => observation,
            Err(_) => {
                let error = "machine observations unavailable: probe timed out".to_string();
                let changed = {
                    let mut state = self.state.lock().await;
                    let previous = state.machine_recovery.phase;
                    state.machine_recovery.observe(
                        &Err(error.clone()),
                        &config.machine_warnings,
                        Instant::now(),
                        SystemTime::now(),
                    );
                    previous != state.machine_recovery.phase
                };
                if changed {
                    self.send_event_raw(Event {
                        id: "machine-recovery".into(),
                        msg: EventMsg::Warning(WarningEvent {
                            message:
                                "Machine recovery monitoring unavailable; stability window reset."
                                    .into(),
                        }),
                    })
                    .await;
                }
                Err(error)
            }
        }
    }

    async fn observe_machine_recovery<F, Fut>(&self, observe: F) -> Result<MachineStatus, String>
    where
        F: FnOnce(ObservationRequest) -> Fut,
        Fut: Future<Output = Result<MachineStatus, String>>,
    {
        // Serialize UI/monitor/model observations, without locking session state during probes.
        let serial = self.state.lock().await.machine_recovery.probe.clone();
        let _probe = serial.lock().await;
        let config = self.get_config().await;
        let cwd = self.state.lock().await.session_configuration.cwd.clone();
        let request = ObservationRequest::new(&config, &cwd);
        let old_wait = {
            let mut state = self.state.lock().await;
            let recovery = &mut state.machine_recovery;
            if recovery.context.as_ref() != Some(&request) {
                let old = if recovery.wait_id.is_some() {
                    recovery.cancel_wait()
                } else {
                    None
                };
                let owner_revision = recovery.owner_revision;
                let sample_revision = recovery.sample_revision;
                *recovery = Recovery::default();
                recovery.owner_revision = owner_revision;
                recovery.sample_revision = sample_revision;
                recovery.probe = serial.clone();
                recovery.context = Some(request.clone());
                old
            } else {
                None
            }
        };
        if let Some(id) = old_wait {
            self.services
                .internal_task_store
                .cancel_machine_wake(&id)
                .await;
        }
        if !config.machine_warnings.enabled {
            // Explicit resource reads still report observations when automatic
            // warnings/monitoring are disabled.
            return observe(request).await;
        }
        let observation = observe(request.clone()).await;
        let current_config = self.get_config().await;
        let (notice, wake) = {
            let mut state = self.state.lock().await;
            if ObservationRequest::new(&current_config, &state.session_configuration.cwd) != request
            {
                return Err("machine observation context changed".into());
            }
            let recovery = &mut state.machine_recovery;
            let previous = recovery.phase;
            recovery.observe(
                &observation,
                &config.machine_warnings,
                Instant::now(),
                SystemTime::now(),
            );
            let notice = (previous != recovery.phase).then_some(match recovery.phase {
                Phase::Recovered => "Machine recovery confirmed. A registered wait can wake the model to reassess; commands are not restarted.",
                Phase::Stabilizing => "Machine conditions improving; observing the recovery stability window.",
                Phase::Unavailable => "Machine recovery monitoring unavailable; stability window reset.",
                Phase::Blocked => "Machine recovery target exceeds filesystem capacity; adjust configuration or storage.",
                Phase::Warning => "Machine warning active. Recovery is not confirmed.",
                Phase::Healthy => "Machine monitoring active.",
            });
            let wake = recovery.parked.then(|| recovery.wait_id.clone()).flatten();
            (notice, wake)
        };
        if let Some(message) = notice {
            self.send_event_raw(Event {
                id: "machine-recovery".into(),
                msg: EventMsg::Warning(WarningEvent {
                    message: message.into(),
                }),
            })
            .await;
        }
        if let Some(id) = wake {
            // Recheck under the state lock to serialize cancellation with publication.
            let state = self.state.lock().await;
            if state.machine_recovery.wait_id.as_ref() == Some(&id) {
                if state.machine_recovery.phase == Phase::Recovered {
                    self.services.internal_task_store.complete(
                        &id,
                        TaskState::Succeeded,
                        Some("Stable recovery observed; reassess before resuming.".into()),
                        Some(serde_json::json!({
                            "recovery": state.machine_recovery.status(&config.machine_warnings, Instant::now()),
                            "observations": observation.as_ref().ok(),
                        })),
                    ).await;
                } else if let Some(mut task) = self.services.internal_task_store.get(&id).await
                    && task.state == TaskState::Succeeded
                    && !task.delivered
                {
                    // Recovery may regress while a ready wake waits behind an
                    // active turn/journal barrier. Withdraw that undelivered
                    // success; later completion must carry fresh observations.
                    task.state = TaskState::Running;
                    task.result = None;
                    task.status_message =
                        Some("Recovery regressed; stability window reset.".into());
                    self.services.internal_task_store.register(task).await;
                }
            }
        }
        observation
    }

    /// Invoked after all tool results in one sample have entered the journal.
    pub(crate) async fn finish_machine_wait_request(&self, turn: &TurnContext, tool_count: usize) {
        let requested = self
            .state
            .lock()
            .await
            .machine_recovery
            .requested_turn
            .as_deref()
            == Some(&turn.sub_id);
        if !requested {
            return;
        }
        if tool_count != 1 {
            self.cancel_machine_recovery_wait().await;
            let item = DeveloperInstructions::new(
                "Recovery wait was not activated: wait_for_machine_recovery must be the only tool call in a model response. Finish checkpointing and checking background work first, then call it alone."
            ).into();
            self.record_conversation_items(turn, &[item]).await;
            return;
        }
        let mut state = self.state.lock().await;
        if state.machine_recovery.requested_turn.as_deref() == Some(&turn.sub_id) {
            state.machine_recovery.requested_turn = None;
            let Some(id) = state.machine_recovery.wait_id.clone() else {
                return;
            };
            state.machine_recovery.parked = true;
            let now = jiff::Timestamp::now().to_string();
            // Outstanding work keeps non-interactive clients from declaring the
            // parked session quiescent before the monitor has a result.
            self.services
                .internal_task_store
                .register(BackgroundTask {
                    id,
                    source: Some(TaskSource::MachineRecovery),
                    state: TaskState::Running,
                    status_message: Some("Waiting for stable machine recovery.".into()),
                    created_at: now.clone(),
                    updated_at: now,
                    result: None,
                    origin_call_id: None,
                    origin_turn_id: Some(turn.sub_id.clone()),
                    execution_id: None,
                    ready: true,
                    notify: true,
                    delivered: false,
                })
                .await;
            drop(state);
            self.send_event_raw(Event {
                id: "machine-recovery".into(),
                msg: EventMsg::Warning(WarningEvent {
                    message: "Recovery wake registered. Model work is parked; existing processes are not stopped. Operator input cancels this live-session wait.".into(),
                }),
            }).await;
        }
    }

    /// The runner calls this before admitting any automatic completion turn.
    pub(crate) async fn machine_recovery_allows_wake(&self) -> bool {
        {
            let state = self.state.lock().await;
            if !state.machine_recovery.parked {
                return true;
            }
            if state.machine_recovery.phase != Phase::Recovered {
                return false;
            }
        }
        if self.refresh_machine_recovery().await.is_err() {
            return false;
        }
        let state = self.state.lock().await;
        state.machine_recovery.parked && state.machine_recovery.phase == Phase::Recovered
    }
}

#[cfg(test)]
mod tests {
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
        session
            .state
            .lock()
            .await
            .machine_recovery
            .request_wait(&turn.sub_id)
            .unwrap();
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
        session.cancel_machine_recovery_wait().await;
        assert!(!session.machine_recovery_parked().await);
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
}
