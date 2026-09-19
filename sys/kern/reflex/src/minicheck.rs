use crate::error::ReflexError;
use crate::judgment::Judgment;
use crate::judgment::JudgmentKind;
use crate::judgment::Verdict;
use crate::local_chat::ChatMessage;
use crate::local_chat::LocalChat;
use crate::router::ReflexBackend;
use crate::router::ReflexFuture;

/// System prompt the chat-style MiniCheck models were trained with.
pub const SYSTEM_PROMPT: &str = "Determine whether the provided claim is consistent with the corresponding document. Consistency in this context implies that all information presented in the claim is substantiated by the document. If not, it should be considered inconsistent. Please assess the claim's consistency with the document by responding with either \"Yes\" or \"No\".";

/// MiniCheck grounding over OpenAI-compatible chat.
#[derive(Debug, Clone)]
pub struct MiniCheckBackend {
    name: String,
    chat: LocalChat,
}

impl MiniCheckBackend {
    pub const DEFAULT_MODEL: &'static str = "bespoke-minicheck";

    pub fn new(name: impl Into<String>, chat: LocalChat) -> Self {
        Self {
            name: name.into(),
            chat,
        }
    }

    pub(crate) fn messages(document: &str, claim: &str) -> [ChatMessage; 2] {
        [
            ChatMessage {
                role: "system",
                content: SYSTEM_PROMPT.to_string(),
            },
            ChatMessage {
                role: "user",
                content: format!("Document: {document}\nClaim: {claim}"),
            },
        ]
    }
}

impl ReflexBackend for MiniCheckBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn supports(&self, kind: JudgmentKind) -> bool {
        kind == JudgmentKind::Grounding
    }

    fn judge(&self, judgment: Judgment) -> ReflexFuture<'_> {
        Box::pin(async move {
            let Judgment::Grounding { document, claim } = judgment else {
                return Err(ReflexError::Unsupported(judgment.kind()));
            };
            let messages = Self::messages(&document, &claim);
            let answer = self.chat.yes_no(&self.name, &messages).await?;
            Verdict::binary(&self.name, answer.probability)
        })
    }
}
