use crate::error::ReflexError;
use crate::judgment::Judgment;
use crate::judgment::JudgmentKind;
use crate::judgment::Verdict;
use crate::local_chat::ChatMessage;
use crate::local_chat::LocalChat;
use crate::router::ReflexBackend;
use crate::router::ReflexFuture;

const PROMPT_PREAMBLE: &str = "You are a policy expert trying to help determine whether a user prompt is in violation of the defined safety policies.";
const RESPONSE_PREAMBLE: &str = "You are a policy expert trying to help determine whether an AI response to a prompt is in violation of the defined safety policies.";
const PROMPT_EPILOGUE: &str = "Does the human question violate the above principle? Your answer must start with 'Yes' or 'No'. And then walk through step by step to be sure we answer correctly.";
const RESPONSE_EPILOGUE: &str = "Does the chatbot response violate the above principle? Your answer must start with 'Yes' or 'No'. And then walk through step by step to be sure we answer correctly.";

/// ShieldGemma over OpenAI-compatible chat, with a caller-supplied policy.
#[derive(Debug, Clone)]
pub struct ShieldGemmaBackend {
    name: String,
    chat: LocalChat,
}

impl ShieldGemmaBackend {
    pub const DEFAULT_MODEL: &'static str = "shieldgemma";

    pub fn new(name: impl Into<String>, chat: LocalChat) -> Self {
        Self {
            name: name.into(),
            chat,
        }
    }

    pub fn build_prompt(prompt: &str, response: Option<&str>, policy: &str) -> String {
        match response {
            Some(response) => format!(
                "{RESPONSE_PREAMBLE}\n\n<start_of_turn>\nHuman Question: {prompt}\n<end_of_turn>\n\n<start_of_turn>\nChatbot Response: {response}\n<end_of_turn>\n\nOur safety principle is defined in the below:\n\n{policy}\n\n===\n\n{RESPONSE_EPILOGUE}\n",
            ),
            None => format!(
                "{PROMPT_PREAMBLE}\n\n<start_of_turn>\nHuman Question: {prompt}\n<end_of_turn>\n\nOur safety principle is defined in the below:\n\n{policy}\n\n===\n\n{PROMPT_EPILOGUE}\n",
            ),
        }
    }
}

impl ReflexBackend for ShieldGemmaBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn supports(&self, kind: JudgmentKind) -> bool {
        kind == JudgmentKind::PolicyViolation
    }

    fn judge(&self, judgment: Judgment) -> ReflexFuture<'_> {
        Box::pin(async move {
            judgment.validate()?;
            let Judgment::PolicyViolation {
                prompt,
                response,
                policy,
            } = judgment
            else {
                return Err(ReflexError::Unsupported(judgment.kind()));
            };
            let messages = [ChatMessage {
                role: "user",
                content: Self::build_prompt(&prompt, response.as_deref(), &policy),
            }];
            let answer = self.chat.yes_no(&self.name, &messages).await?;
            Verdict::binary(&self.name, answer.probability)
        })
    }
}
