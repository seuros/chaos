use std::time::Duration;

use chaos_client::ChaosHttpClient;
use serde::Deserialize;
use serde::Serialize;

use crate::error::ReflexError;

const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";
const TOP_LOGPROBS: u8 = 5;

/// Non-streaming OpenAI-compatible chat client.
#[derive(Clone)]
pub struct LocalChat {
    http: ChaosHttpClient,
    base_url: String,
    model: String,
    api_key: Option<String>,
    timeout: Duration,
}

impl std::fmt::Debug for LocalChat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalChat")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatMessage {
    pub role: &'static str,
    pub content: String,
}

/// Probability of "yes" from a Yes/No classifier's first token.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct YesNo {
    pub probability: f64,
    /// Derived from logprobs rather than the emitted token.
    pub soft: bool,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
    temperature: f32,
    max_tokens: u32,
    logprobs: bool,
    top_logprobs: u8,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
    #[serde(default)]
    logprobs: Option<ChoiceLogprobs>,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    #[serde(default)]
    content: String,
}

#[derive(Deserialize)]
struct ChoiceLogprobs {
    #[serde(default)]
    content: Vec<TokenLogprob>,
}

#[derive(Deserialize)]
struct TokenLogprob {
    #[serde(default)]
    top_logprobs: Vec<TopLogprob>,
}

#[derive(Deserialize)]
pub(crate) struct TopLogprob {
    pub(crate) token: String,
    pub(crate) logprob: f64,
}

impl LocalChat {
    pub fn new(
        http: ChaosHttpClient,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let base_url: String = base_url.into();
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.into(),
            api_key: None,
            timeout: crate::DEFAULT_TIMEOUT,
        }
    }

    pub fn with_api_key(mut self, api_key: Option<String>) -> Self {
        self.api_key = api_key.filter(|key| !key.trim().is_empty());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Send `messages` to a Yes/No classifier and read the first token.
    pub async fn yes_no(
        &self,
        backend: &str,
        messages: &[ChatMessage],
    ) -> Result<YesNo, ReflexError> {
        let url = format!("{}{CHAT_COMPLETIONS_PATH}", self.base_url);
        let body = ChatRequest {
            model: &self.model,
            messages,
            stream: false,
            temperature: 0.0,
            max_tokens: 1,
            logprobs: true,
            top_logprobs: TOP_LOGPROBS,
        };
        let mut request = self.http.post(&url).json(&body);
        if let Some(api_key) = &self.api_key {
            request = request.bearer_auth(api_key);
        }
        let response = request
            .timeout(self.timeout)
            .execute()
            .await
            .map_err(|err| ReflexError::transport(backend, err))?;
        let parsed: ChatResponse = serde_json::from_slice(&response.body)
            .map_err(|err| ReflexError::malformed(backend, err.to_string()))?;
        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ReflexError::malformed(backend, "response carried no choices"))?;
        let first_token = choice
            .logprobs
            .and_then(|logprobs| logprobs.content.into_iter().next())
            .map(|token| token.top_logprobs)
            .unwrap_or_default();
        if let Some(probability) = yes_no_from_logprobs(&first_token) {
            return Ok(YesNo {
                probability,
                soft: true,
            });
        }
        Ok(YesNo {
            probability: yes_no_from_text(backend, &choice.message.content)?,
            soft: false,
        })
    }
}

/// P(yes) normalized over Yes/No candidates; `None` if absent or invalid.
pub(crate) fn yes_no_from_logprobs(candidates: &[TopLogprob]) -> Option<f64> {
    let labels: Vec<(bool, f64)> = candidates
        .iter()
        .filter_map(|candidate| label_of(&candidate.token).map(|label| (label, candidate.logprob)))
        .collect();
    if labels
        .iter()
        .any(|(_, logprob)| !logprob.is_finite() || *logprob > 0.0)
    {
        return None;
    }
    let max = labels
        .iter()
        .map(|(_, logprob)| *logprob)
        .reduce(f64::max)?;
    let mut yes = 0.0;
    let mut no = 0.0;
    for (label, logprob) in labels {
        if label {
            yes += (logprob - max).exp();
        } else {
            no += (logprob - max).exp();
        }
    }
    let total = yes + no;
    (total > 0.0).then(|| yes / total)
}

/// Map a Yes/No token to a probability of "yes".
pub(crate) fn yes_no_from_text(backend: &str, text: &str) -> Result<f64, ReflexError> {
    let head = text.trim_start().trim_start_matches(['*', '"', '\'', '`']);
    let word: String = head
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_ascii_lowercase();
    match word.as_str() {
        "yes" => Ok(1.0),
        "no" => Ok(0.0),
        _ => Err(ReflexError::malformed(
            backend,
            format!(
                "expected Yes or No, got {:?}",
                head.chars().take(32).collect::<String>()
            ),
        )),
    }
}

fn label_of(token: &str) -> Option<bool> {
    match token.trim().to_ascii_lowercase().as_str() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}
