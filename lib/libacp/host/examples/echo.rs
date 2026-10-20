use std::sync::Arc;

use acp_host::protocol::schema::v1::{
    ContentBlock, ContentChunk, NewSessionRequest, PromptRequest, PromptResponse, SessionUpdate,
    StopReason, TextContent,
};
use acp_host::{AgentBackend, AgentServer, AgentSession, SessionContext, protocol};

struct Echo;

impl AgentBackend for Echo {
    fn new_session(
        &self,
        _request: NewSessionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<Arc<dyn AgentSession>>> {
        Box::pin(async { Ok(Arc::new(Echo) as Arc<dyn AgentSession>) })
    }
}

impl AgentSession for Echo {
    fn prompt(
        &self,
        request: PromptRequest,
        context: SessionContext,
    ) -> protocol::BoxFuture<'_, protocol::Result<PromptResponse>> {
        Box::pin(async move {
            for content in request.prompt {
                if let ContentBlock::Text(text) = content {
                    context.client().update(SessionUpdate::AgentMessageChunk(
                        ContentChunk::new(ContentBlock::Text(TextContent::new(text.text))),
                    ))?;
                }
            }
            Ok(PromptResponse::new(StopReason::EndTurn))
        })
    }

    fn cancel(&self) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn shutdown(&self) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), acp_host::HostError> {
    AgentServer::builder(Echo)
        .build()?
        .serve(tokio::io::stdin(), tokio::io::stdout())
        .await
}
