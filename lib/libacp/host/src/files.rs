use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use protocol::JsonRpcRequest;
use protocol::schema::v1::{ReadTextFileRequest, WriteTextFileRequest};

use crate::state::{Slot, error, invalid};
use crate::{ClientHandle, MAX_EDITOR_TEXT_BYTES, protocol};

struct UnconfirmedWrite(Option<Arc<Slot>>);

impl Drop for UnconfirmedWrite {
    fn drop(&mut self) {
        if let Some(slot) = &self.0 {
            slot.invalidate();
        }
    }
}

impl ClientHandle {
    /// Read current editor text. Capability errors never select a disk fallback.
    pub async fn read_text_file(&self, path: &Path) -> protocol::Result<String> {
        if !path.is_absolute() || !self.capabilities()?.fs.read_text_file {
            return Err(invalid(
                "editor text reads are unavailable or path is not absolute",
            ));
        }
        let reply = self
            .client_request(
                ReadTextFileRequest::new(self.session_id.clone(), path),
                false,
            )
            .await?;
        if reply.content.len() > MAX_EDITOR_TEXT_BYTES {
            return Err(error("editor text exceeds the byte budget"));
        }
        Ok(reply.content)
    }

    /// Delegate text replacement to the client; no separate save is requested.
    /// The client's filesystem API controls persistence, not the agent. A sent
    /// write without a successful acknowledgement invalidates this session:
    /// its effect is unknown, not retryable.
    pub async fn write_text_file(&self, path: &Path, content: String) -> protocol::Result<()> {
        if !path.is_absolute() || !self.capabilities()?.fs.write_text_file {
            return Err(invalid(
                "editor text writes are unavailable or path is not absolute",
            ));
        }
        if content.len() > MAX_EDITOR_TEXT_BYTES {
            return Err(invalid("editor text exceeds the byte budget"));
        }
        self.client_request(
            WriteTextFileRequest::new(self.session_id.clone(), path, content),
            true,
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn client_request<R: JsonRpcRequest>(
        &self,
        request: R,
        write: bool,
    ) -> protocol::Result<R::Response> {
        let (state, _) = self.ready()?;
        self.client_request_with_timeout(
            request,
            write,
            state.limits.approval_timeout.min(Duration::from_secs(15)),
        )
        .await
    }

    pub(crate) async fn client_request_with_timeout<R: JsonRpcRequest>(
        &self,
        request: R,
        write: bool,
        timeout: Duration,
    ) -> protocol::Result<R::Response> {
        let (state, slot) = self.ready()?;
        let message = request.to_untyped_message()?;
        let frame = serde_json::json!({
            "jsonrpc":"2.0", "id":"reserved-request-id-budget",
            "method":message.method, "params":message.params,
        });
        if serde_json::to_vec(&frame)
            .map_err(|_| error("editor request could not be encoded"))?
            .len()
            .saturating_add(128)
            > state.limits.max_message_bytes
        {
            return Err(invalid("editor request exceeds the frame budget"));
        }
        if self.turn.cancel.is_cancelled() {
            return Err(error("editor request cancelled before dispatch"));
        }
        let permit = state
            .requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| error("ACP client request limit exceeded"))?;
        let (raw_tx, raw_rx) = tokio::sync::oneshot::channel();
        let received = state.clone();
        {
            let active = slot.lock();
            self.check_ready(&state, &active)?;
            if self.turn.cancel.is_cancelled() {
                return Err(error("editor request cancelled before dispatch"));
            }
            self.connection
                .prepare_request(request)
                .on_receiving_result(async move |reply| {
                    received.acknowledge_inbound();
                    drop(permit);
                    // The caller may have cancelled; reply accounting still completes.
                    let _ = raw_tx.send(reply);
                    Ok(())
                })?;
        }
        let mut unknown = UnconfirmedWrite(write.then_some(slot));
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.connection.spawn(async move {
            if let Ok(reply) = tokio::time::timeout(timeout, raw_rx).await {
                // The caller may already have cancelled or dropped its receiver.
                let _ = tx.send(reply);
            }
            Ok(())
        })?;
        let result = tokio::select! {
            _ = self.turn.cancel.cancelled() => {
                return Err(error("editor request cancelled; a sent write outcome is unknown"));
            }
            _ = state.stop.cancelled() => {
                return Err(error("editor connection closed; a sent write outcome is unknown"));
            }
            response = rx => response,
        };
        match result {
            Ok(Ok(Ok(reply))) if self.ready().is_ok() && !self.turn.cancel.is_cancelled() => {
                unknown.0 = None;
                Ok(reply)
            }
            Ok(Ok(Err(reply)))
                if !write && self.ready().is_ok() && !self.turn.cancel.is_cancelled() =>
            {
                Err(reply)
            }
            _ => Err(error(
                "editor request failed; a sent write outcome is unknown",
            )),
        }
    }
}
