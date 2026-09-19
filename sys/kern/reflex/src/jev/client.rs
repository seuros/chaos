use std::time::Duration;

use chaos_client::ChaosHttpClient;
use chaos_client::RetryOn;
use chaos_client::RetryPolicy;
use chaos_client::run_with_retry;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::debug;

use super::error::JevError;
use super::types::ModelsResponse;
use super::types::Questions;
use super::types::SystemOneRequest;
use super::types::SystemOneResponse;

pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";
pub const DEFAULT_MODEL: &str = "jev-latest";
pub const DEFAULT_ENV_KEY: &str = "TYPESAFE_API_KEY";

/// Path of the decisions endpoint under `base_url`.
pub const DEFAULT_PATH: &str = "/v1/systemone";
const MODELS_PATH: &str = "/v1/models";
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const DEFAULT_BASE_DELAY: Duration = Duration::from_millis(200);

/// Non-streaming client for the System One endpoint.
#[derive(Clone)]
pub struct JevClient {
    http: ChaosHttpClient,
    base_url: String,
    api_key: String,
    model: String,
    path: String,
    timeout: Duration,
    max_attempts: u32,
    base_delay: Duration,
}

impl std::fmt::Debug for JevClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JevClient")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl JevClient {
    pub fn new(
        http: ChaosHttpClient,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        let base_url: String = base_url.into();
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: DEFAULT_MODEL.to_string(),
            path: DEFAULT_PATH.to_string(),
            timeout: crate::DEFAULT_TIMEOUT,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            base_delay: DEFAULT_BASE_DELAY,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Use another decisions path, such as OpenRouter's `/alpha/decisions`.
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        let path: String = path.into();
        let trimmed = path.trim().trim_end_matches('/');
        self.path = if trimmed.starts_with('/') {
            trimmed.to_string()
        } else {
            format!("/{trimmed}")
        };
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_retry(mut self, max_attempts: u32, base_delay: Duration) -> Self {
        self.max_attempts = max_attempts.max(1);
        self.base_delay = base_delay;
        self
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Evaluate `questions` against `state` in one request.
    pub async fn evaluate(
        &self,
        state: Value,
        questions: Questions,
    ) -> Result<SystemOneResponse, JevError> {
        if questions.is_empty() {
            return Err(JevError::NoQuestions);
        }
        for (key, question) in &questions {
            question.validate(key)?;
        }
        let body = SystemOneRequest {
            state,
            model: self.model.clone(),
            questions,
        };
        let url = format!("{}{}", self.base_url, self.path);
        let response: SystemOneResponse = self
            .send_with_retry(|| self.http.post(&url).json(&body))
            .await?;
        response.validate(&body.questions)?;
        debug!(
            model = %response.model,
            answers = response.answers.len(),
            input_tokens = response.usage.input_tokens,
            "system one evaluation completed"
        );
        Ok(response)
    }

    /// List the model routes the endpoint currently serves.
    pub async fn list_models(&self) -> Result<ModelsResponse, JevError> {
        let url = format!("{}{MODELS_PATH}", self.base_url);
        self.send_with_retry(|| self.http.get(&url)).await
    }

    async fn send_with_retry<T: DeserializeOwned>(
        &self,
        build: impl Fn() -> chaos_client::ChaosRequestBuilder,
    ) -> Result<T, JevError> {
        let response = run_with_retry(
            RetryPolicy {
                max_attempts: u64::from(self.max_attempts - 1),
                base_delay: self.base_delay,
                retry_on: RetryOn {
                    retry_429: true,
                    retry_5xx: true,
                    retry_transport: true,
                },
            },
            || build().bearer_auth(&self.api_key).timeout(self.timeout),
            |request, _| request.execute(),
        )
        .await
        .map_err(|error| JevError::from_transport(error, self.timeout))?;
        serde_json::from_slice(&response.body).map_err(|err| JevError::Decode(err.to_string()))
    }
}
