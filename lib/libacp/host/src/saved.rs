use std::sync::Arc;

use protocol::schema::v1::{LoadSessionRequest, ResumeSessionRequest, SessionId};
use protocol::{Client, ConnectionTo};
use tokio::sync::OwnedSemaphorePermit;

use crate::state::{State, error, invalid};
use crate::{AgentBackend, SessionInfo, protocol};

pub(crate) enum SavedRequest {
    Load(LoadSessionRequest),
    Resume(ResumeSessionRequest),
}

impl SavedRequest {
    pub fn id(&self) -> &SessionId {
        match self {
            Self::Load(request) => &request.session_id,
            Self::Resume(request) => &request.session_id,
        }
    }
}

pub(crate) fn admit(state: &State, id: &SessionId) -> protocol::Result<OwnedSemaphorePermit> {
    state.ready()?;
    if state.find(id).is_ok() {
        return Err(invalid("ACP session is already open"));
    }
    state
        .session_permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| error("ACP session limit exceeded"))
}

pub(crate) async fn open(
    state: Arc<State>,
    backend: Arc<dyn AgentBackend>,
    request: SavedRequest,
    permit: OwnedSemaphorePermit,
    connection: ConnectionTo<Client>,
) -> protocol::Result<SessionInfo> {
    let id = request.id().clone();
    let replay = matches!(&request, SavedRequest::Load(_));
    let native = match request {
        SavedRequest::Load(request) => backend.load_session(request).await?,
        SavedRequest::Resume(request) => backend.resume_session(request).await?,
    };
    let (_, guard, info) = state.open(native, permit, Some(&id)).await?;
    if replay {
        let context = guard.context(&state, &id, connection);
        if let Err(error) = guard.slot.native.replay(context).await {
            guard.slot.invalidate();
            state.shutdown_slot(&guard.slot).await?;
            return Err(error);
        }
    }
    drop(guard);
    Ok(info)
}
