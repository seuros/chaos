use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use protocol::schema::ProtocolVersion;
use protocol::schema::v1::{
    AgentCapabilities, AuthenticateRequest, CancelNotification, ContentBlock, Implementation,
    InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    SessionId,
};
use protocol::{Agent, Lines};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::state::{ClientHandle, ServeGuard, Slot, State, Turn, TurnGuard, error, invalid};
use crate::{AgentBackend, ConnectionLimits, HostError, SessionContext, framing, protocol};

/// A reusable ACP agent host. Native execution belongs to its backend.
pub struct AgentServer {
    backend: Arc<dyn AgentBackend>,
    info: Implementation,
    limits: ConnectionLimits,
}

/// Configuration for one independently served connection.
pub struct AgentServerBuilder {
    inner: AgentServer,
}

impl AgentServer {
    /// Select a native backend; default limits are conservative and bounded.
    pub fn builder(backend: impl AgentBackend) -> AgentServerBuilder {
        AgentServerBuilder {
            inner: Self {
                backend: Arc::new(backend),
                info: Implementation::new("acp-host", env!("CARGO_PKG_VERSION")),
                limits: ConnectionLimits::default(),
            },
        }
    }

    /// Serve one ACP client connection over caller-owned byte streams.
    ///
    /// Dropping this future closes transport and schedules bounded native cleanup.
    pub async fn serve<R, W>(&self, reader: R, writer: W) -> Result<(), HostError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        if !self.limits.valid() {
            return Err(HostError::InvalidLimits);
        }
        let state = State::new(self.limits.clone());
        let _guard = ServeGuard(state.clone());
        let init = state.clone();
        let opening = state.clone();
        let prompting = state.clone();
        let cancelling = state.clone();
        let auth = state.clone();
        let backend = self.backend.clone();
        let prompt_capabilities = self.backend.prompt_capabilities();
        let capabilities =
            AgentCapabilities::new().prompt_capabilities(prompt_capabilities.clone());
        let info = self.info.clone();
        let builder = Agent
            .builder()
            .name("acp-host")
            .on_receive_request(
                async move |_request: InitializeRequest, responder, _cx| {
                    if init.initialized.swap(true, Ordering::AcqRel) {
                        return responder.respond_with_error(invalid("ACP already initialized"));
                    }
                    responder.respond(
                        InitializeResponse::new(ProtocolVersion::V1)
                            .agent_capabilities(capabilities.clone())
                            .agent_info(info.clone()),
                    )
                },
                protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |_request: AuthenticateRequest, responder, _cx| {
                    if let Err(error) = auth.ready() {
                        return responder.respond_with_error(error);
                    }
                    responder
                        .respond_with_error(invalid("no interactive ACP authentication method"))
                },
                protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: NewSessionRequest, responder, cx| {
                    if let Err(error) = opening.ready() {
                        return responder.respond_with_error(error);
                    }
                    if !request.cwd.is_absolute() {
                        return responder.respond_with_error(invalid("ACP cwd must be absolute"));
                    }
                    let permit = match opening.session_permits.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            return responder
                                .respond_with_error(error("ACP session limit exceeded"));
                        }
                    };
                    let backend = backend.clone();
                    let opening = opening.clone();
                    cx.spawn(async move {
                        let native = match backend.new_session(request).await {
                            Ok(native) => native,
                            Err(error) => return responder.respond_with_error(error),
                        };
                        let id = SessionId::new(uuid::Uuid::new_v4().to_string());
                        let slot = Arc::new(Slot {
                            native,
                            turn: Mutex::new(None),
                            queued: AtomicUsize::new(0),
                            invalid: AtomicBool::new(false),
                            _permit: permit,
                        });
                        if opening.stop.is_cancelled() {
                            slot.native.shutdown().await?;
                            return responder.respond_with_error(error("ACP connection closed"));
                        }
                        opening
                            .sessions
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .insert(id.clone(), slot);
                        responder.respond(NewSessionResponse::new(id))
                    })
                },
                protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: PromptRequest, responder, cx| {
                    let supported = request.prompt.iter().all(|content| match content {
                        ContentBlock::Text(_) | ContentBlock::ResourceLink(_) => true,
                        ContentBlock::Image(_) => prompt_capabilities.image,
                        ContentBlock::Audio(_) => prompt_capabilities.audio,
                        ContentBlock::Resource(_) => prompt_capabilities.embedded_context,
                        _ => false,
                    });
                    if !supported {
                        return responder.respond_with_error(invalid("unsupported prompt content"));
                    }
                    let slot = match prompting.find(&request.session_id) {
                        Ok(slot) => slot,
                        Err(error) => return responder.respond_with_error(error),
                    };
                    if slot.invalid.load(Ordering::Acquire) {
                        return responder.respond_with_error(error("ACP session is invalid"));
                    }
                    let turn = Turn::new();
                    {
                        let mut active = slot.lock_turn();
                        if active.is_some() {
                            return responder.respond_with_error(error("ACP session is busy"));
                        }
                        *active = Some(turn.clone());
                    }
                    let context = SessionContext {
                        client: ClientHandle {
                            session_id: request.session_id.clone(),
                            connection: cx.clone(),
                            state: Arc::downgrade(&prompting),
                            slot: Arc::downgrade(&slot),
                            turn: turn.clone(),
                        },
                    };
                    let guard = TurnGuard {
                        slot: slot.clone(),
                        turn,
                    };
                    cx.spawn(async move {
                        let result = slot.native.prompt(request, context).await;
                        if result.is_err() {
                            slot.invalid.store(true, Ordering::Release);
                        }
                        {
                            let _active = slot.lock_turn();
                            guard.turn.live.store(false, Ordering::Release);
                        }
                        // A late cancellation callback must never interrupt the next
                        // turn. Keep this session busy until that callback has joined.
                        if guard.turn.cancel_started.load(Ordering::Acquire) {
                            guard.turn.cancel_done.cancelled().await;
                        }
                        drop(guard);
                        responder.respond_with_result(result)
                    })
                },
                protocol::on_receive_request!(),
            )
            .on_receive_notification(
                async move |request: CancelNotification, cx| {
                    cancelling.acknowledge_inbound();
                    if let Ok(slot) = cancelling.find(&request.session_id) {
                        let active = slot
                            .lock_turn()
                            .as_ref()
                            .filter(|turn| {
                                turn.live.load(Ordering::Acquire)
                                    && !turn.cancel_started.swap(true, Ordering::AcqRel)
                            })
                            .cloned();
                        if let Some(turn) = active {
                            turn.cancel.cancel();
                            let state = cancelling.clone();
                            cx.spawn(async move {
                                let result = tokio::time::timeout(
                                    state.limits.shutdown_timeout,
                                    slot.native.cancel(),
                                )
                                .await;
                                if !matches!(result, Ok(Ok(()))) {
                                    state.fail("ACP native cancellation is unconfirmed");
                                }
                                turn.cancel_done.cancel();
                                Ok(())
                            })?;
                        }
                    }
                    Ok(())
                },
                protocol::on_receive_notification!(),
            );
        let driver = builder.connect_to(Lines::new(
            framing::outgoing(writer, state.clone()),
            framing::incoming(reader, state.clone()),
        ));
        let result = tokio::select! {
            result = driver => result.map_err(HostError::Protocol),
            _ = state.stop.cancelled() => Ok(()),
        };
        state.cleanup().await?;
        if let Some(reason) = *state.failure.lock().unwrap_or_else(PoisonError::into_inner) {
            return Err(HostError::Closed(reason));
        }
        result
    }
}

impl AgentServerBuilder {
    /// Set the agent identity advertised during initialization.
    pub fn identity(mut self, name: impl Into<String>, version: impl Into<String>) -> Self {
        self.inner.info = Implementation::new(name, version);
        self
    }

    /// Override finite resource limits.
    pub fn limits(mut self, limits: ConnectionLimits) -> Self {
        self.inner.limits = limits;
        self
    }

    /// Validate configuration before transport or native work starts.
    pub fn build(self) -> Result<AgentServer, HostError> {
        if !self.inner.limits.valid() {
            return Err(HostError::InvalidLimits);
        }
        Ok(self.inner)
    }
}
