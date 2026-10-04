//! Policy evaluation, rule application, and 401 recovery state machines.

use std::sync::Arc;

use crate::auth::ChaosAuth;
use crate::auth::ExternalAuthRefreshReason;
use crate::auth::RefreshTokenError;
use crate::error::RefreshTokenFailedError;
use crate::error::RefreshTokenFailedReason;
use state_machines::runtime::{ActivityId, Machine, Runner};
use state_machines::state_machine;
use tokio::sync::oneshot;

use super::tokens::AuthManager;
use super::tokens::ReloadOutcome;

const REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE: &str = "Your access token could not be refreshed because you have since logged out or signed in to another account. Please sign in again.";

// UnauthorizedRecovery is a state machine that handles 401 recovery.
//
// Managed mode (ChatGPT or xAI auth):
//   Reload → RefreshToken → Done
// External mode (external ChatGPT auth tokens):
//   ExternalRefresh → Done
// API key auth: no recovery available.
state_machine! {
    name: ManagedRecovery,
    dynamic: true,
    initial: Reload,
    states: [Reload, RefreshToken, Done],
    final_states: [Done],
    events {
        reloaded {
            transition: { from: Reload, to: RefreshToken }
        }
        reload_skipped {
            transition: { from: Reload, to: Done }
        }
        refreshed {
            transition: { from: RefreshToken, to: Done }
        }
        refresh_failed {
            transition: { from: RefreshToken, to: RefreshToken }
        }
    }
}

state_machine! {
    name: ExternalRecovery,
    dynamic: true,
    initial: Pending,
    states: [Pending, Completed],
    final_states: [Completed],
    events {
        refreshed {
            transition: { from: Pending, to: Completed }
        }
        refresh_failed {
            transition: { from: Pending, to: Pending }
        }
    }
}

pub(super) enum RecoveryMachine {
    Managed(Runner<DynamicManagedRecovery<()>>),
    External(Runner<DynamicExternalRecovery<()>>),
}

pub struct UnauthorizedRecovery {
    pub(super) manager: Arc<AuthManager>,
    pub(super) machine: RecoveryMachine,
    pub(super) expected_account_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnauthorizedRecoveryStepResult {
    auth_state_changed: Option<bool>,
}

impl UnauthorizedRecoveryStepResult {
    pub fn auth_state_changed(&self) -> Option<bool> {
        self.auth_state_changed
    }
}

impl UnauthorizedRecovery {
    pub(super) fn new(manager: Arc<AuthManager>) -> Self {
        let cached_auth = manager.auth_cached();
        let expected_account_id = cached_auth.as_ref().and_then(ChaosAuth::get_account_id);
        let machine = if cached_auth
            .as_ref()
            .is_some_and(ChaosAuth::is_external_chatgpt_tokens)
        {
            RecoveryMachine::External(Runner::new(DynamicExternalRecovery::new(()), 2))
        } else {
            RecoveryMachine::Managed(Runner::new(DynamicManagedRecovery::new(()), 2))
        };

        Self {
            manager,
            machine,
            expected_account_id,
        }
    }

    pub fn has_next(&self) -> bool {
        self.unavailable_reason() == "ready"
    }

    pub fn unavailable_reason(&self) -> &'static str {
        if !self
            .manager
            .auth_cached()
            .as_ref()
            .is_some_and(ChaosAuth::supports_unauthorized_recovery)
        {
            return "unsupported_auth";
        }

        if let RecoveryMachine::External(_) = &self.machine
            && !self.manager.has_external_auth_refresher()
        {
            return "no_external_refresher";
        }

        let is_done = match &self.machine {
            RecoveryMachine::Managed(m) => m.machine().is_finished(),
            RecoveryMachine::External(m) => m.machine().is_finished(),
        };
        if is_done {
            return "recovery_exhausted";
        }

        "ready"
    }

    pub fn mode_name(&self) -> &'static str {
        match &self.machine {
            RecoveryMachine::Managed(_) => "managed",
            RecoveryMachine::External(_) => "external",
        }
    }

    pub fn step_name(&self) -> &'static str {
        match &self.machine {
            RecoveryMachine::Managed(m) => match m.machine().current_state() {
                ManagedRecoveryState::Reload => "reload",
                ManagedRecoveryState::RefreshToken => "refresh_token",
                _ => "done",
            },
            RecoveryMachine::External(m) => match m.machine().current_state() {
                ExternalRecoveryState::Pending => "external_refresh",
                _ => "done",
            },
        }
    }

    pub async fn next(&mut self) -> Result<UnauthorizedRecoveryStepResult, RefreshTokenError> {
        if !self.has_next() {
            return Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                "No more recovery steps available.",
            )));
        }

        let manager = self.manager.clone();
        let (reply, result) = oneshot::channel();
        match &mut self.machine {
            RecoveryMachine::Managed(runner) => {
                let state = runner.machine().current_state();
                let expected_account_id = self.expected_account_id.clone();
                run_step(runner, result, async move {
                    let (event, result) = match state {
                        ManagedRecoveryState::Reload => {
                            match manager
                                .reload_if_account_id_matches(expected_account_id.as_deref())
                            {
                                ReloadOutcome::ReloadedChanged => {
                                    (ManagedRecoveryEvent::Reloaded, changed(true))
                                }
                                ReloadOutcome::ReloadedNoChange => {
                                    (ManagedRecoveryEvent::Reloaded, changed(false))
                                }
                                ReloadOutcome::Skipped => (
                                    ManagedRecoveryEvent::ReloadSkipped,
                                    Err(RefreshTokenError::Permanent(
                                        RefreshTokenFailedError::new(
                                            RefreshTokenFailedReason::Other,
                                            REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE.to_string(),
                                        ),
                                    )),
                                ),
                            }
                        }
                        ManagedRecoveryState::RefreshToken => {
                            match manager.refresh_token_from_authority().await {
                                Ok(()) => (ManagedRecoveryEvent::Refreshed, changed(true)),
                                Err(error) => (ManagedRecoveryEvent::RefreshFailed, Err(error)),
                            }
                        }
                        _ => unreachable!("completed recovery cannot invoke a step"),
                    };
                    let _ = reply.send(result);
                    event
                })
                .await
            }
            RecoveryMachine::External(runner) => {
                run_step(runner, result, async move {
                    let (event, result) = match manager
                        .refresh_external_auth(ExternalAuthRefreshReason::Unauthorized)
                        .await
                    {
                        Ok(()) => (ExternalRecoveryEvent::Refreshed, changed(true)),
                        Err(error) => (ExternalRecoveryEvent::RefreshFailed, Err(error)),
                    };
                    let _ = reply.send(result);
                    event
                })
                .await
            }
        }
    }
}

type StepResult = Result<UnauthorizedRecoveryStepResult, RefreshTokenError>;

fn changed(changed: bool) -> StepResult {
    Ok(UnauthorizedRecoveryStepResult {
        auth_state_changed: Some(changed),
    })
}

struct RecoveryInvocation<'a, M: Machine> {
    runner: &'a mut Runner<M>,
    activity: ActivityId,
}

impl<M: Machine> Drop for RecoveryInvocation<'_, M> {
    fn drop(&mut self) {
        self.runner.cancel_activity(self.activity);
    }
}

async fn run_step<M: Machine>(
    runner: &mut Runner<M>,
    mut result: oneshot::Receiver<StepResult>,
    work: impl Future<Output = M::Event> + Send + 'static,
) -> StepResult
where
    M::Error: std::fmt::Debug,
{
    let activity = runner
        .invoke_future(work)
        .map_err(|error| std::io::Error::other(format!("auth recovery admission: {error:?}")))?;
    let invocation = RecoveryInvocation { runner, activity };
    loop {
        invocation
            .runner
            .drain(4)
            .await
            .map_err(|error| std::io::Error::other(format!("auth recovery dispatch: {error:?}")))?;
        match result.try_recv() {
            Ok(result) => return result,
            Err(oneshot::error::TryRecvError::Empty) => {
                invocation.runner.wait_for_work().await.map_err(|error| {
                    std::io::Error::other(format!("auth recovery work: {error:?}"))
                })?;
            }
            Err(oneshot::error::TryRecvError::Closed) => {
                unreachable!("auth recovery completion owns its reply")
            }
        }
    }
}
