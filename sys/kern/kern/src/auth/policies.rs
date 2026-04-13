//! Policy evaluation, rule application, and 401 recovery state machines.

use std::sync::Arc;

use crate::auth::ChaosAuth;
use crate::auth::ExternalAuthRefreshReason;
use crate::auth::RefreshTokenError;
use crate::error::RefreshTokenFailedError;
use crate::error::RefreshTokenFailedReason;
use state_machines::state_machine;

use super::tokens::AuthManager;
use super::tokens::ReloadOutcome;

const REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE: &str = "Your access token could not be refreshed because you have since logged out or signed in to another account. Please sign in again.";

// UnauthorizedRecovery is a state machine that handles 401 recovery.
//
// Managed mode (ChatGPT auth):
//   Reload → RefreshToken → Done
// External mode (external ChatGPT auth tokens):
//   ExternalRefresh → Done
// API key auth: no recovery available.
state_machine! {
    name: ManagedRecovery,
    dynamic: true,
    initial: Reload,
    states: [Reload, RefreshToken, Done],
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
    }
}

state_machine! {
    name: ExternalRecovery,
    dynamic: true,
    initial: Pending,
    states: [Pending, Completed],
    events {
        refreshed {
            transition: { from: Pending, to: Completed }
        }
    }
}

pub(super) enum RecoveryMachine {
    Managed(DynamicManagedRecovery<()>),
    External(DynamicExternalRecovery<()>),
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
            RecoveryMachine::External(DynamicExternalRecovery::new(()))
        } else {
            RecoveryMachine::Managed(DynamicManagedRecovery::new(()))
        };

        Self {
            manager,
            machine,
            expected_account_id,
        }
    }

    pub fn has_next(&self) -> bool {
        if !self
            .manager
            .auth_cached()
            .as_ref()
            .is_some_and(ChaosAuth::is_chatgpt_auth)
        {
            return false;
        }

        match &self.machine {
            RecoveryMachine::External(m) => {
                if !self.manager.has_external_auth_refresher() {
                    return false;
                }
                m.current_state() != "Completed"
            }
            RecoveryMachine::Managed(m) => m.current_state() != "Done",
        }
    }

    pub fn unavailable_reason(&self) -> &'static str {
        if !self
            .manager
            .auth_cached()
            .as_ref()
            .is_some_and(ChaosAuth::is_chatgpt_auth)
        {
            return "not_chatgpt_auth";
        }

        if let RecoveryMachine::External(_) = &self.machine
            && !self.manager.has_external_auth_refresher()
        {
            return "no_external_refresher";
        }

        let is_done = match &self.machine {
            RecoveryMachine::Managed(m) => m.current_state() == "Done",
            RecoveryMachine::External(m) => m.current_state() == "Completed",
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
            RecoveryMachine::Managed(m) => match m.current_state() {
                "Reload" => "reload",
                "RefreshToken" => "refresh_token",
                _ => "done",
            },
            RecoveryMachine::External(m) => match m.current_state() {
                "Pending" => "external_refresh",
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

        match &mut self.machine {
            RecoveryMachine::Managed(m) => match m.current_state() {
                "Reload" => {
                    match self
                        .manager
                        .reload_if_account_id_matches(self.expected_account_id.as_deref())
                    {
                        ReloadOutcome::ReloadedChanged => {
                            let _ = m.handle(ManagedRecoveryEvent::Reloaded);
                            Ok(UnauthorizedRecoveryStepResult {
                                auth_state_changed: Some(true),
                            })
                        }
                        ReloadOutcome::ReloadedNoChange => {
                            let _ = m.handle(ManagedRecoveryEvent::Reloaded);
                            Ok(UnauthorizedRecoveryStepResult {
                                auth_state_changed: Some(false),
                            })
                        }
                        ReloadOutcome::Skipped => {
                            let _ = m.handle(ManagedRecoveryEvent::ReloadSkipped);
                            Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                                RefreshTokenFailedReason::Other,
                                REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE.to_string(),
                            )))
                        }
                    }
                }
                "RefreshToken" => {
                    self.manager.refresh_token_from_authority().await?;
                    let _ = m.handle(ManagedRecoveryEvent::Refreshed);
                    Ok(UnauthorizedRecoveryStepResult {
                        auth_state_changed: Some(true),
                    })
                }
                _ => Ok(UnauthorizedRecoveryStepResult {
                    auth_state_changed: None,
                }),
            },
            RecoveryMachine::External(m) => {
                self.manager
                    .refresh_external_auth(ExternalAuthRefreshReason::Unauthorized)
                    .await?;
                let _ = m.handle(ExternalRecoveryEvent::Refreshed);
                Ok(UnauthorizedRecoveryStepResult {
                    auth_state_changed: Some(true),
                })
            }
        }
    }
}
