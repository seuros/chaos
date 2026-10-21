use std::sync::{Arc, PoisonError};

use protocol::schema::v1::{
    AvailableCommandsUpdate, ConfigOptionUpdate, CurrentModeUpdate, SessionId, SessionUpdate,
    SetSessionConfigOptionRequest,
};
use protocol::{Client, ConnectionTo};
use tokio::sync::OwnedSemaphorePermit;

use crate::state::{ClientHandle, Slot, State, Turn, TurnGuard, error, invalid};
use crate::{AgentSession, SessionContext, SessionInfo, protocol};

impl State {
    pub(crate) async fn shutdown_slot(&self, slot: &Slot) -> protocol::Result<()> {
        match tokio::time::timeout(self.limits.shutdown_timeout, slot.native.shutdown()).await {
            Ok(Ok(())) => Ok(()),
            _ => {
                self.fail("ACP native cleanup is unconfirmed");
                Err(error("ACP native cleanup is unconfirmed"))
            }
        }
    }

    pub(crate) async fn open(
        self: &Arc<Self>,
        native: Arc<dyn AgentSession>,
        permit: OwnedSemaphorePermit,
        expected_id: Option<&SessionId>,
    ) -> protocol::Result<(SessionId, TurnGuard, SessionInfo)> {
        // Own cleanup before any backend metadata await can fail or be dropped.
        let turn = Turn::new_operation(false);
        // The Opening state owns admission through metadata and history replay.
        let slot = Slot::opening(native, turn.clone(), permit);
        let mut opening = Opening {
            state: self.clone(),
            slot: slot.clone(),
            armed: true,
        };
        let mut info = match slot.native.info().await {
            Ok(info) => info,
            Err(error) => {
                opening.shutdown().await?;
                return Err(error);
            }
        };
        self.filter_info(&mut info);
        let id = info
            .id
            .clone()
            .unwrap_or_else(|| SessionId::new(uuid::Uuid::new_v4().to_string()));
        if expected_id.is_some_and(|expected| expected != &id) {
            opening.shutdown().await?;
            return Err(invalid("ACP backend returned a different saved session"));
        }
        let rejected = {
            let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
            if self.ready().is_err() || sessions.contains_key(&id) {
                true
            } else {
                sessions.insert(id.clone(), slot.clone());
                false
            }
        };
        if rejected {
            opening.shutdown().await?;
            return Err(error("ACP session already open or connection closed"));
        }
        opening.armed = false;
        Ok((id, TurnGuard { slot, turn }, info))
    }

    pub(crate) fn operation(
        self: &Arc<Self>,
        id: &SessionId,
        connection: ConnectionTo<Client>,
        prompt: bool,
    ) -> protocol::Result<(SessionContext, TurnGuard)> {
        let slot = self.find(id)?;
        let turn = Turn::new_operation(prompt);
        {
            let mut session = slot.lock();
            self.ready()?;
            if prompt {
                session.begin(turn.clone())?;
            } else {
                session.begin_control(turn.clone())?;
            }
        }
        let guard = TurnGuard { slot, turn };
        Ok((guard.context(self, id, connection), guard))
    }
}

impl TurnGuard {
    pub(crate) fn context(
        &self,
        state: &Arc<State>,
        id: &SessionId,
        connection: ConnectionTo<Client>,
    ) -> SessionContext {
        SessionContext {
            client: ClientHandle {
                session_id: id.clone(),
                connection,
                state: Arc::downgrade(state),
                slot: Arc::downgrade(&self.slot),
                turn: self.turn.clone(),
            },
        }
    }
}

struct Opening {
    state: Arc<State>,
    slot: Arc<Slot>,
    armed: bool,
}

impl Opening {
    async fn shutdown(&mut self) -> protocol::Result<()> {
        let result = self.state.shutdown_slot(&self.slot).await;
        self.armed = false;
        result
    }
}

impl Drop for Opening {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let state = self.state.clone();
            let slot = self.slot.clone();
            runtime.spawn(async move {
                // shutdown_slot records unconfirmed cleanup in the connection state.
                let _ = state.shutdown_slot(&slot).await;
            });
        }
    }
}

pub(crate) async fn configure(
    request: SetSessionConfigOptionRequest,
    context: SessionContext,
    guard: TurnGuard,
) -> protocol::Result<SessionInfo> {
    let result = guard
        .slot
        .native
        .set_config_option(request, context.clone())
        .await;
    if result
        .as_ref()
        .is_err_and(|error| error.code == protocol::Error::internal_error().code)
    {
        guard.slot.invalidate();
    }
    let mut info = result?;
    context.client().ready()?.0.filter_info(&mut info);
    publish_info(&info, &context)?;
    Ok(info)
}

pub(crate) fn publish_info(info: &SessionInfo, context: &SessionContext) -> protocol::Result<()> {
    if let Some(options) = &info.config_options {
        context
            .client()
            .update(SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(
                options.clone(),
            )))?;
    }
    if let Some(modes) = &info.modes {
        context
            .client()
            .update(SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new(
                modes.current_mode_id.clone(),
            )))?;
    }
    Ok(())
}

pub(crate) async fn publish_commands(
    info: &SessionInfo,
    context: &SessionContext,
) -> protocol::Result<()> {
    if let Some(commands) = &info.available_commands {
        context
            .client()
            .update_wait(SessionUpdate::AvailableCommandsUpdate(
                AvailableCommandsUpdate::new(commands.clone()),
            ))
            .await?;
    }
    Ok(())
}
