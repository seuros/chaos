use std::sync::Arc;

use protocol::schema::ProtocolVersion;
use protocol::schema::v1::{
    AgentCapabilities, AuthenticateRequest, CancelNotification, ContentBlock, Implementation,
    InitializeRequest, InitializeResponse, ListSessionsRequest, LoadSessionRequest,
    LoadSessionResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    ResumeSessionRequest, ResumeSessionResponse, SessionCapabilities, SessionConfigOptionValue,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse, SetSessionModeRequest,
    SetSessionModeResponse,
};
use protocol::{Agent, Lines};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::state::{ServeGuard, State, error, invalid};
use crate::{AgentBackend, ConnectionLimits, HostError, framing, operations, protocol, saved};

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
        let configuring = state.clone();
        let modes = state.clone();
        let loading = state.clone();
        let resuming = state.clone();
        let listing = state.clone();
        let load_backend = self.backend.clone();
        let resume_backend = self.backend.clone();
        let list_backend = self.backend.clone();
        let backend = self.backend.clone();
        let prompt_capabilities = self.backend.prompt_capabilities();
        let backend_capabilities = self.backend.session_capabilities();
        // Do not forward close/delete (or future methods) without dispatch and
        // lifecycle implementations. Backend declarations are not handlers.
        let session_capabilities = SessionCapabilities::new()
            .list(backend_capabilities.list)
            .resume(backend_capabilities.resume)
            .additional_directories(backend_capabilities.additional_directories)
            .meta(backend_capabilities.meta);
        let can_list = session_capabilities.list.is_some();
        let can_resume = session_capabilities.resume.is_some();
        let can_load = self.backend.load_session_supported();
        let capabilities = AgentCapabilities::new()
            .prompt_capabilities(prompt_capabilities.clone())
            .load_session(can_load)
            .session_capabilities(session_capabilities);
        let info = self.info.clone();
        let builder = Agent
            .builder()
            .name("acp-host")
            .on_receive_request(
                async move |request: InitializeRequest, responder, _cx| {
                    if let Err(error) = init.initialize(request.client_capabilities) {
                        return responder.respond_with_error(error);
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
                async move |request: ListSessionsRequest, responder, cx| {
                    if let Err(error) = listing.ready() {
                        return responder.respond_with_error(error);
                    }
                    if !can_list {
                        return responder
                            .respond_with_error(invalid("ACP session listing is unsupported"));
                    }
                    let backend = list_backend.clone();
                    cx.spawn(async move {
                        responder.respond_with_result(backend.list_sessions(request).await)
                    })
                },
                protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: LoadSessionRequest, responder, cx| {
                    if !can_load {
                        return responder
                            .respond_with_error(invalid("ACP session loading is unsupported"));
                    }
                    if let Err(error) =
                        saved::validate_paths(&request.cwd, &request.additional_directories)
                    {
                        return responder.respond_with_error(error);
                    }
                    let permit = match saved::admit(&loading, &request.session_id) {
                        Ok(permit) => permit,
                        Err(error) => return responder.respond_with_error(error),
                    };
                    let state = loading.clone();
                    let backend = load_backend.clone();
                    let connection = cx.clone();
                    cx.spawn(async move {
                        let result = saved::open(
                            state,
                            backend,
                            saved::SavedRequest::Load(request),
                            permit,
                            connection,
                        )
                        .await
                        .map(|info| {
                            LoadSessionResponse::new()
                                .config_options(info.config_options)
                                .modes(info.modes)
                        });
                        responder.respond_with_result(result)
                    })
                },
                protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: ResumeSessionRequest, responder, cx| {
                    if !can_resume {
                        return responder
                            .respond_with_error(invalid("ACP session resume is unsupported"));
                    }
                    if let Err(error) =
                        saved::validate_paths(&request.cwd, &request.additional_directories)
                    {
                        return responder.respond_with_error(error);
                    }
                    let permit = match saved::admit(&resuming, &request.session_id) {
                        Ok(permit) => permit,
                        Err(error) => return responder.respond_with_error(error),
                    };
                    let state = resuming.clone();
                    let backend = resume_backend.clone();
                    let connection = cx.clone();
                    cx.spawn(async move {
                        let result = saved::open(
                            state,
                            backend,
                            saved::SavedRequest::Resume(request),
                            permit,
                            connection,
                        )
                        .await
                        .map(|info| {
                            ResumeSessionResponse::new()
                                .config_options(info.config_options)
                                .modes(info.modes)
                        });
                        responder.respond_with_result(result)
                    })
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
                    if let Err(error) =
                        saved::validate_paths(&request.cwd, &request.additional_directories)
                    {
                        return responder.respond_with_error(error);
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
                    let connection = cx.clone();
                    cx.spawn(async move {
                        let native = match backend.new_session(request).await {
                            Ok(native) => native,
                            Err(error) => return responder.respond_with_error(error),
                        };
                        let (id, guard, info) = match opening.open(native, permit, None).await {
                            Ok(opened) => opened,
                            Err(error) => return responder.respond_with_error(error),
                        };
                        if let Err(error) = operations::publish_commands(
                            &info,
                            &guard.context(&opening, &id, connection),
                        )
                        .await
                        {
                            guard.slot.invalidate();
                            opening.shutdown_slot(&guard.slot).await?;
                            return responder.respond_with_error(error);
                        }
                        responder.respond(
                            NewSessionResponse::new(id)
                                .config_options(info.config_options)
                                .modes(info.modes),
                        )
                    })
                },
                protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: SetSessionConfigOptionRequest, responder, cx| {
                    if matches!(request.value, SessionConfigOptionValue::Boolean { .. })
                        && !configuring.supports_boolean()
                    {
                        return responder.respond_with_error(invalid(
                            "boolean configuration is unsupported by this client",
                        ));
                    }
                    let (context, guard) =
                        match configuring.operation(&request.session_id, cx.clone(), false) {
                            Ok(operation) => operation,
                            Err(error) => return responder.respond_with_error(error),
                        };
                    cx.spawn(async move {
                        let result =
                            operations::configure(request, context, guard)
                                .await
                                .map(|info| {
                                    SetSessionConfigOptionResponse::new(
                                        info.config_options.unwrap_or_default(),
                                    )
                                });
                        responder.respond_with_result(result)
                    })
                },
                protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: SetSessionModeRequest, responder, cx| {
                    let (context, guard) =
                        match modes.operation(&request.session_id, cx.clone(), false) {
                            Ok(operation) => operation,
                            Err(error) => return responder.respond_with_error(error),
                        };
                    cx.spawn(async move {
                        let change = SetSessionConfigOptionRequest::new(
                            request.session_id,
                            "mode",
                            request.mode_id.0.as_ref(),
                        );
                        let result = operations::configure(change, context, guard)
                            .await
                            .map(|_| SetSessionModeResponse::new());
                        responder.respond_with_result(result)
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
                    let (context, guard) =
                        match prompting.operation(&request.session_id, cx.clone(), true) {
                            Ok(operation) => operation,
                            Err(error) => return responder.respond_with_error(error),
                        };
                    cx.spawn(async move {
                        let slot = &guard.slot;
                        let mut result = slot.native.prompt(request, context).await;
                        if result.is_ok() && slot.is_invalid() {
                            result =
                                Err(error("ACP session has an unconfirmed editor write outcome"));
                        }
                        if result.is_err() {
                            slot.invalidate();
                        }
                        // A late cancellation callback must never interrupt the next
                        // turn. Keep this session busy until that callback has joined.
                        if guard.complete() {
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
                    if let Ok(slot) = cancelling.find(&request.session_id) {
                        let active = slot.lock().request_cancel();
                        if let Some(turn) = active {
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
                                turn.cancel_finished();
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
        if let Some(reason) = state.failure_reason() {
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
