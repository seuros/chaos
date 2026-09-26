//! ChaOS owns the system prompt; AGY supplies authentication and transport.
//! Rewrite only supported generation requests, never OAuth or control traffic.

use std::sync::{Arc, RwLock};

use rama::{
    error::BoxError,
    http::{
        Body, HeaderValue, Method, Request,
        body::util::{BodyExt, Limited},
        header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, TRANSFER_ENCODING},
    },
};
use serde_json::{Value, json};

const MAX_GENERATION_REQUEST_BYTES: usize = 16 * 1024 * 1024;

/// Shared by the proxy handle and all of its connections. Updated before the
/// CLI starts each turn, so resumed conversations cannot retain a stale prompt.
#[derive(Clone)]
pub(super) struct AntigravitySystemPrompt(Arc<RwLock<Arc<str>>>);

impl std::fmt::Debug for AntigravitySystemPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AntigravitySystemPrompt(<redacted>)")
    }
}

impl AntigravitySystemPrompt {
    pub(super) fn new(text: String) -> Self {
        Self(Arc::new(RwLock::new(Arc::from(text))))
    }

    pub(super) fn update(&self, text: String) -> Result<(), BoxError> {
        *self
            .0
            .write()
            .map_err(|_| "clamp system prompt lock poisoned")? = Arc::from(text);
        Ok(())
    }

    pub(super) async fn rewrite(&self, req: Request, host: &str) -> Result<Request, BoxError> {
        let path = req.uri().path_or_root();
        let Some(envelope) = generation_envelope(host, &path)? else {
            return Ok(req);
        };
        if req.method() != Method::POST {
            return Err("clamp generation requests must use POST".into());
        }
        if req
            .headers()
            .get_all(CONTENT_ENCODING)
            .iter()
            .any(|encoding| {
                encoding
                    .to_str()
                    .map_or(true, |value| !value.eq_ignore_ascii_case("identity"))
            })
        {
            return Err(
                "clamp cannot replace system instructions in encoded generation requests".into(),
            );
        }
        let content_type = req
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .unwrap_or_default()
            .trim();
        if !content_type.eq_ignore_ascii_case("application/json") {
            return Err("clamp generation requests must use JSON".into());
        }
        let instructions = self
            .0
            .read()
            .map_err(|_| "clamp system prompt lock poisoned")?
            .clone();
        let (mut parts, body) = req.into_parts();
        let bytes = Limited::new(body, MAX_GENERATION_REQUEST_BYTES)
            .collect()
            .await?
            .to_bytes();
        let mut payload: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "clamp generation request is not valid JSON")?;
        let request = match envelope {
            GenerationEnvelope::CloudCode => payload.get_mut("request"),
            GenerationEnvelope::Gemini => Some(&mut payload),
        }
        .and_then(Value::as_object_mut)
        .ok_or("clamp generation request has an unsupported envelope")?;
        if !request.get("contents").is_some_and(Value::is_array) {
            return Err("clamp generation request is missing contents".into());
        }
        // An explicit cache can contain an old system prompt that cannot be
        // replaced without reconstructing its contents. Never silently use it.
        if request.contains_key("cachedContent") || request.contains_key("cached_content") {
            return Err(
                "clamp cannot replace system instructions in cached-content requests".into(),
            );
        }
        request.remove("systemInstruction");
        request.remove("system_instruction");
        if !instructions.is_empty() {
            request.insert(
                "systemInstruction".to_string(),
                json!({"parts": [{"text": instructions.as_ref()}]}),
            );
        }
        let bytes = serde_json::to_vec(&payload)?;
        // Neither framing nor body digests from the CLI describe the new body.
        for name in [
            TRANSFER_ENCODING.as_str(),
            CONTENT_ENCODING.as_str(),
            "content-md5",
            "digest",
            "content-digest",
        ] {
            parts.headers.remove(name);
        }
        parts.headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&bytes.len().to_string())?,
        );
        Ok(Request::from_parts(parts, Body::from(bytes)))
    }
}

enum GenerationEnvelope {
    CloudCode,
    Gemini,
}

fn generation_envelope(host: &str, path: &str) -> Result<Option<GenerationEnvelope>, BoxError> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    match (host.as_str(), path) {
        (
            "cloudcode-pa.googleapis.com" | "daily-cloudcode-pa.googleapis.com",
            "/v1internal:generateContent" | "/v1internal:streamGenerateContent",
        ) => return Ok(Some(GenerationEnvelope::CloudCode)),
        ("generativelanguage.googleapis.com", path) => {
            let model_path = path
                .strip_prefix("/v1beta/models/")
                .or_else(|| path.strip_prefix("/v1/models/"));
            if let Some((model, action)) = model_path.and_then(|path| path.split_once(':'))
                && !model.is_empty()
                && !model.contains('/')
                && matches!(action, "generateContent" | "streamGenerateContent")
            {
                return Ok(Some(GenerationEnvelope::Gemini));
            }
        }
        _ => {}
    }
    // Refuse other versions, batch/live generation and gRPC generation rather
    // than forwarding a recognizable inference call with the CLI's prompt.
    if path.to_ascii_lowercase().contains("generatecontent") {
        return Err("clamp generation endpoint does not support system-prompt replacement".into());
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
