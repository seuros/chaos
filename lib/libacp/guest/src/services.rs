use std::collections::HashMap;
use std::sync::Arc;
use std::sync::{Mutex, PoisonError};

use crate::{ConnectionOptions, permissions::PendingPermissions, protocol};
use protocol::schema::v1::*;
use protocol::{Agent, Client};
use tokio::sync::Semaphore;

/// Editor read policy and implementation. Authorize session and canonical paths
/// (including symlinks) before accessing data; honor line/limit semantics.
pub trait FileReader: std::fmt::Debug + Send + Sync {
    fn read_text_file(
        &self,
        request: ReadTextFileRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<ReadTextFileResponse>>;
}

/// Editor write policy and implementation. Authorize before side effects.
/// Futures must be cancellation-safe; dropping one does not prove no write occurred.
pub trait FileWriter: std::fmt::Debug + Send + Sync {
    fn write_text_file(
        &self,
        request: WriteTextFileRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<WriteTextFileResponse>>;
}

/// Application-owned terminal lifecycle. Implement all five methods before
/// opting in. Bind handles to sessions, enforce command/environment policy,
/// truncate output at UTF-8 boundaries and release/kill/reap processes yourself.
/// Service futures may be dropped at deadlines or disconnect; the application
/// must retain ownership of partially created terminals and arrange cleanup.
pub trait TerminalHandler: std::fmt::Debug + Send + Sync {
    fn create(
        &self,
        request: CreateTerminalRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<CreateTerminalResponse>>;
    fn output(
        &self,
        request: TerminalOutputRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<TerminalOutputResponse>>;
    fn release(
        &self,
        request: ReleaseTerminalRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<ReleaseTerminalResponse>>;
    fn wait_for_exit(
        &self,
        request: WaitForTerminalExitRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<WaitForTerminalExitResponse>>;
    fn kill(
        &self,
        request: KillTerminalRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<KillTerminalResponse>>;
}

/// User-owned form/URL UI. Advertise only modes actually rendered. Validate the
/// session/request scope; never open URLs or collect secrets without user policy.
/// A future is dropped on session cancellation, deadline or disconnect.
pub trait ElicitationHandler: std::fmt::Debug + Send + Sync {
    fn capabilities(&self) -> ElicitationCapabilities;
    fn create(
        &self,
        request: CreateElicitationRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<CreateElicitationResponse>>;
    fn complete(
        &self,
        _notification: CompleteElicitationNotification,
    ) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

fn unavailable() -> protocol::Error {
    protocol::Error::invalid_params().data("ACP client service unavailable or invalid path")
}

#[derive(Clone)]
pub(crate) struct Services {
    options: ConnectionOptions,
    pending: PendingPermissions,
    permits: Arc<Semaphore>,
    elicitation: Option<ElicitationCapabilities>,
    urls: Arc<Mutex<HashMap<ElicitationId, Arc<()>>>>,
    state: crate::ConnectionState,
}

// Register concrete SDK types; every handler yields before application work.
macro_rules! service {
    ($builder:expr, $services:expr, $request:ty, $method:ident) => {{
        let services = $services.clone();
        $builder.on_receive_request(
            async move |request: $request, responder, cx| {
                let permit = match services.permits.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        return responder.respond_with_error(
                            protocol::Error::internal_error()
                                .data("ACP client service limit exceeded"),
                        );
                    }
                };
                let services = services.clone();
                cx.spawn(async move {
                    let _permit = permit;
                    let result = tokio::time::timeout(
                        services.options.request_timeout,
                        services.$method(request),
                    )
                    .await
                    .unwrap_or_else(|_| {
                        Err(protocol::Error::internal_error()
                            .data("ACP client service deadline; outcome may be unknown"))
                    });
                    responder.respond_with_result(result)
                })
            },
            protocol::on_receive_request!(),
        )
    }};
}

impl Services {
    pub fn new(
        options: &ConnectionOptions,
        pending: PendingPermissions,
        state: crate::ConnectionState,
    ) -> Self {
        Self {
            options: options.clone(),
            pending,
            permits: Arc::new(Semaphore::new(options.pending_services)),
            elicitation: options
                .elicitation
                .as_ref()
                .map(|handler| handler.capabilities()),
            urls: Arc::new(Mutex::new(HashMap::new())),
            state,
        }
    }

    pub fn capabilities(&self) -> ClientCapabilities {
        ClientCapabilities::new()
            .fs(FileSystemCapabilities::new()
                .read_text_file(self.options.file_reader.is_some())
                .write_text_file(self.options.file_writer.is_some()))
            .terminal(self.options.terminals.is_some())
            .elicitation(self.elicitation.clone())
    }

    pub fn builder(self) -> protocol::Builder<Client, impl protocol::HandleDispatchFrom<Agent>> {
        let builder = service!(Client.builder(), self, ReadTextFileRequest, read);
        let builder = service!(builder, self, WriteTextFileRequest, write);
        let builder = service!(builder, self, CreateTerminalRequest, create_terminal);
        let builder = service!(builder, self, TerminalOutputRequest, output);
        let builder = service!(builder, self, ReleaseTerminalRequest, release);
        let builder = service!(builder, self, WaitForTerminalExitRequest, wait);
        let builder = service!(builder, self, KillTerminalRequest, kill);
        let completions = self
            .elicitation
            .as_ref()
            .is_some_and(|caps| caps.url.is_some())
            .then(|| self.options.elicitation.clone())
            .flatten();
        let completion_permits = self.permits.clone();
        let completion_timeout = self.options.request_timeout;
        let completed_urls = self.urls.clone();
        let completion_state = self.state.clone();
        builder.on_receive_request(
            async move |request: CreateElicitationRequest, responder, cx| {
                let supported = self.elicitation.as_ref().is_some_and(|caps| match &request.mode {
                    ElicitationMode::Form(_) => caps.form.is_some(),
                    ElicitationMode::Url(_) => caps.url.is_some(),
                    _ => false,
                });
                if !supported {
                    return responder.respond_with_error(unavailable());
                }
                let permit = match self.permits.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => return responder.respond_with_error(unavailable()),
                };
                let url = if let ElicitationMode::Url(mode) = &request.mode {
                    let mut urls = self.urls.lock().unwrap_or_else(PoisonError::into_inner);
                    if urls.len() >= self.options.pending_services || urls.contains_key(&mode.elicitation_id) {
                        return responder.respond_with_error(unavailable());
                    }
                    let ticket = Arc::new(());
                    urls.insert(mode.elicitation_id.clone(), ticket.clone());
                    Some((mode.elicitation_id.clone(), ticket))
                } else {
                    None
                };
                // Register before yielding, so a subsequently queued cancel cannot
                // race an application approval into the outgoing queue.
                let pending = match request.scope() {
                    ElicitationScope::Session(scope) => Some(self.pending.register(scope.session_id.clone())),
                    _ => None,
                };
                let handler = self.options.elicitation.clone();
                let timeout = self.options.request_timeout;
                let urls = self.urls.clone();
                cx.spawn(async move {
                    let _permit = permit;
                    let clear = || {
                        if let Some((id, ticket)) = &url {
                            let mut urls = urls.lock().unwrap_or_else(PoisonError::into_inner);
                            if urls.get(id).is_some_and(|pending| Arc::ptr_eq(pending, ticket)) {
                                urls.remove(id);
                            }
                        }
                    };
                    let cancel = || {
                        clear();
                        Ok(CreateElicitationResponse::new(ElicitationAction::Cancel))
                    };
                    let result = tokio::select! {
                        biased;
                        _ = async { match &pending {
                            Some(pending) => pending.cancelled().await,
                            None => std::future::pending().await,
                        }} => cancel(),
                        result = async {
                            let handler = handler.ok_or_else(unavailable)?;
                            tokio::time::timeout(timeout, handler.create(request)).await.unwrap_or_else(|_| cancel())
                        } => result,
                    };
                    if !matches!(&result, Ok(reply) if matches!(reply.action, ElicitationAction::Accept(_))) {
                        clear();
                    }
                    match pending {
                        Some(pending) => pending.respond_with(responder, result, cancel),
                        None => responder.respond_with_result(result),
                    }
                })
            },
            protocol::on_receive_request!(),
        ).on_receive_notification(
                async move |notification: CompleteElicitationNotification, cx| {
                    let Some(handler) = completions.clone() else { return Ok(()) };
                    if !completed_urls.lock().unwrap_or_else(PoisonError::into_inner).contains_key(&notification.elicitation_id) {
                        return Ok(());
                    }
                    let permit = match completion_permits.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            completion_state.close();
                            return Err(protocol::Error::internal_error().data("ACP completion service limit exceeded"));
                        }
                    };
                    if completed_urls.lock().unwrap_or_else(PoisonError::into_inner).remove(&notification.elicitation_id).is_none() {
                        return Ok(());
                    }
                    cx.spawn(async move {
                        let _permit = permit;
                        tokio::time::timeout(completion_timeout, handler.complete(notification))
                            .await.map_err(|_| protocol::Error::internal_error().data("ACP completion service deadline"))?
                    })
            },
            protocol::on_receive_notification!(),
        )
    }

    async fn read(&self, request: ReadTextFileRequest) -> protocol::Result<ReadTextFileResponse> {
        if !request.path.is_absolute() {
            return Err(unavailable());
        }
        self.options
            .file_reader
            .as_ref()
            .ok_or_else(unavailable)?
            .read_text_file(request)
            .await
    }

    async fn write(
        &self,
        request: WriteTextFileRequest,
    ) -> protocol::Result<WriteTextFileResponse> {
        if !request.path.is_absolute() {
            return Err(unavailable());
        }
        self.options
            .file_writer
            .as_ref()
            .ok_or_else(unavailable)?
            .write_text_file(request)
            .await
    }

    async fn create_terminal(
        &self,
        request: CreateTerminalRequest,
    ) -> protocol::Result<CreateTerminalResponse> {
        if request.cwd.as_ref().is_some_and(|cwd| !cwd.is_absolute()) {
            return Err(unavailable());
        }
        self.options
            .terminals
            .as_ref()
            .ok_or_else(unavailable)?
            .create(request)
            .await
    }

    async fn output(
        &self,
        request: TerminalOutputRequest,
    ) -> protocol::Result<TerminalOutputResponse> {
        self.options
            .terminals
            .as_ref()
            .ok_or_else(unavailable)?
            .output(request)
            .await
    }
    async fn release(
        &self,
        request: ReleaseTerminalRequest,
    ) -> protocol::Result<ReleaseTerminalResponse> {
        self.options
            .terminals
            .as_ref()
            .ok_or_else(unavailable)?
            .release(request)
            .await
    }
    async fn wait(
        &self,
        request: WaitForTerminalExitRequest,
    ) -> protocol::Result<WaitForTerminalExitResponse> {
        self.options
            .terminals
            .as_ref()
            .ok_or_else(unavailable)?
            .wait_for_exit(request)
            .await
    }
    async fn kill(&self, request: KillTerminalRequest) -> protocol::Result<KillTerminalResponse> {
        self.options
            .terminals
            .as_ref()
            .ok_or_else(unavailable)?
            .kill(request)
            .await
    }
}
