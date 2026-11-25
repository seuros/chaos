//! ChaOS owns the system prompt; AGY supplies authentication and transport.
//! Rewrite only supported generation requests, never OAuth or control traffic.

use std::sync::{
    Arc, RwLock,
    atomic::{AtomicU64, Ordering},
};

use rama::{
    error::BoxError,
    http::{
        Body, HeaderValue, Method, Request,
        body::util::{BodyExt, Limited},
        header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, TRANSFER_ENCODING},
    },
};
use serde_json::{Value, json};

pub(super) const MAX_GENERATION_REQUEST_BYTES: usize = 16 * 1024 * 1024;
/// Maximum total base64 bytes of staged tool images kept in memory for a single
/// Antigravity turn, leaving headroom below [`MAX_GENERATION_REQUEST_BYTES`].
pub(super) const MAX_STAGED_TOOL_IMAGE_BYTES: usize = 12 * 1024 * 1024;

/// Validated, human-attached audio clip authorized for the active Antigravity turn.
#[derive(Clone, PartialEq, Eq)]
pub struct AntigravityAudioAttachment {
    /// Random per-turn nonce embedded in the permanent text placeholder (`id <nonce>`).
    pub nonce: String,
    /// Validated MIME type (for example `"audio/wav"`).
    pub mime_type: String,
    /// Standard base64-encoded audio bytes.
    pub data_base64: Arc<str>,
}

impl std::fmt::Debug for AntigravityAudioAttachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AntigravityAudioAttachment")
            .field("nonce", &self.nonce)
            .field("mime_type", &self.mime_type)
            .field("data_base64_len", &self.data_base64.len())
            .finish()
    }
}

#[derive(Clone)]
struct ActiveAudioTurn {
    generation_id: u64,
    authorized_model: String,
    attachments: Vec<AntigravityAudioAttachment>,
    spliced_requests: usize,
}

/// RAII guard that disarms turn-scoped Antigravity audio when dropped (for
/// example on turn cancellation) or when explicitly disarmed at turn end.
pub struct AntigravityAudioGuard {
    audio: Arc<RwLock<Option<ActiveAudioTurn>>>,
    generation_id: Option<u64>,
    disarmed: bool,
}

impl AntigravityAudioGuard {
    /// Clear the active turn's attachments and return the number of authorized
    /// main generation requests that received the splice.
    pub fn disarm(mut self) -> Result<usize, BoxError> {
        self.disarmed = true;
        let Some(generation_id) = self.generation_id else {
            return Ok(0);
        };
        let mut guard = self
            .audio
            .write()
            .map_err(|_| "clamp audio attachment lock poisoned")?;
        if guard
            .as_ref()
            .is_some_and(|turn| turn.generation_id == generation_id)
        {
            Ok(guard.take().map_or(0, |turn| turn.spliced_requests))
        } else {
            Ok(0)
        }
    }
}

impl Drop for AntigravityAudioGuard {
    fn drop(&mut self) {
        if !self.disarmed
            && let Some(generation_id) = self.generation_id
            && let Ok(mut guard) = self.audio.write()
            && guard
                .as_ref()
                .is_some_and(|turn| turn.generation_id == generation_id)
        {
            *guard = None;
        }
    }
}

/// Validated, tool-returned image staged for the active Antigravity turn.
///
/// Antigravity CLI offloads MCP `ContentBlock::Image` results to disk and tells
/// the model to call native `read_file`, which Chaos denies. Instead, the clamp
/// bridge stages the image here and sends a per-image nonce placeholder; the
/// egress proxy splices `inlineData` into the matching `functionResponse`.
#[derive(Clone, PartialEq, Eq)]
pub struct AntigravityToolImageAttachment {
    /// Random per-image nonce embedded in the bridge placeholder (`[chaos-image id <nonce>]`).
    pub nonce: String,
    /// Validated image MIME type (for example `"image/png"`).
    pub mime_type: String,
    /// Standard base64-encoded image bytes.
    pub data_base64: Arc<str>,
}

impl std::fmt::Debug for AntigravityToolImageAttachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AntigravityToolImageAttachment")
            .field("nonce", &self.nonce)
            .field("mime_type", &self.mime_type)
            .field("data_base64_len", &self.data_base64.len())
            .finish()
    }
}

#[derive(Clone)]
struct ActiveToolImageTurn {
    generation_id: u64,
    authorized_model: String,
    images: Vec<AntigravityToolImageAttachment>,
    total_base64_bytes: usize,
    spliced_requests: usize,
}

/// RAII guard that clears turn-scoped Antigravity tool images when dropped or
/// explicitly disarmed at turn end.
pub struct AntigravityToolImageGuard {
    tool_images: Arc<RwLock<Option<ActiveToolImageTurn>>>,
    generation_id: u64,
    disarmed: bool,
}

impl AntigravityToolImageGuard {
    /// Clear the active turn's staged tool images and return the number of
    /// authorized main generation requests that received an image splice.
    pub fn disarm(mut self) -> Result<usize, BoxError> {
        self.disarmed = true;
        let mut guard = self
            .tool_images
            .write()
            .map_err(|_| "clamp tool image lock poisoned")?;
        if guard
            .as_ref()
            .is_some_and(|turn| turn.generation_id == self.generation_id)
        {
            Ok(guard.take().map_or(0, |turn| turn.spliced_requests))
        } else {
            Ok(0)
        }
    }
}

impl Drop for AntigravityToolImageGuard {
    fn drop(&mut self) {
        if !self.disarmed
            && let Ok(mut guard) = self.tool_images.write()
            && guard
                .as_ref()
                .is_some_and(|turn| turn.generation_id == self.generation_id)
        {
            *guard = None;
        }
    }
}

/// Shared by the proxy handle and all of its connections. Updated before the
/// CLI starts each turn, so resumed conversations cannot retain a stale prompt.
#[derive(Clone)]
pub(super) struct AntigravitySystemPrompt {
    instructions: Arc<RwLock<Arc<str>>>,
    audio: Arc<RwLock<Option<ActiveAudioTurn>>>,
    next_audio_generation: Arc<AtomicU64>,
    tool_images: Arc<RwLock<Option<ActiveToolImageTurn>>>,
    next_tool_image_generation: Arc<AtomicU64>,
}

impl std::fmt::Debug for AntigravitySystemPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AntigravitySystemPrompt(<redacted>)")
    }
}

impl AntigravitySystemPrompt {
    pub(super) fn new(text: String) -> Self {
        Self {
            instructions: Arc::new(RwLock::new(Arc::from(text))),
            audio: Arc::new(RwLock::new(None)),
            next_audio_generation: Arc::new(AtomicU64::new(1)),
            tool_images: Arc::new(RwLock::new(None)),
            next_tool_image_generation: Arc::new(AtomicU64::new(1)),
        }
    }

    pub(super) fn update(&self, text: String) -> Result<(), BoxError> {
        *self
            .instructions
            .write()
            .map_err(|_| "clamp system prompt lock poisoned")? = Arc::from(text);
        Ok(())
    }

    pub(super) fn arm_audio(
        &self,
        authorized_model: String,
        attachments: Vec<AntigravityAudioAttachment>,
    ) -> Result<AntigravityAudioGuard, BoxError> {
        let mut guard = self
            .audio
            .write()
            .map_err(|_| "clamp audio attachment lock poisoned")?;
        let generation_id = if attachments.is_empty() {
            *guard = None;
            None
        } else {
            let generation_id = self.next_audio_generation.fetch_add(1, Ordering::Relaxed);
            *guard = Some(ActiveAudioTurn {
                generation_id,
                authorized_model: normalize_model_name(&authorized_model).to_string(),
                attachments,
                spliced_requests: 0,
            });
            Some(generation_id)
        };
        Ok(AntigravityAudioGuard {
            audio: Arc::clone(&self.audio),
            generation_id,
            disarmed: false,
        })
    }

    pub(super) fn disarm_audio(&self) -> Result<usize, BoxError> {
        let mut guard = self
            .audio
            .write()
            .map_err(|_| "clamp audio attachment lock poisoned")?;
        Ok(guard.take().map_or(0, |turn| turn.spliced_requests))
    }

    pub(super) fn arm_tool_images(
        &self,
        authorized_model: String,
    ) -> Result<AntigravityToolImageGuard, BoxError> {
        let mut guard = self
            .tool_images
            .write()
            .map_err(|_| "clamp tool image lock poisoned")?;
        let generation_id = self
            .next_tool_image_generation
            .fetch_add(1, Ordering::Relaxed);
        *guard = Some(ActiveToolImageTurn {
            generation_id,
            authorized_model: normalize_model_name(&authorized_model).to_string(),
            images: Vec::new(),
            total_base64_bytes: 0,
            spliced_requests: 0,
        });
        Ok(AntigravityToolImageGuard {
            tool_images: Arc::clone(&self.tool_images),
            generation_id,
            disarmed: false,
        })
    }

    pub(super) fn stage_tool_image(
        &self,
        attachment: AntigravityToolImageAttachment,
    ) -> Result<(), BoxError> {
        if attachment.nonce.trim().is_empty() {
            return Err("clamp tool image nonce must not be empty".into());
        }
        if !attachment.mime_type.starts_with("image/") {
            return Err(format!(
                "clamp tool image has unsupported MIME type `{}`",
                attachment.mime_type
            )
            .into());
        }
        if attachment.data_base64.is_empty() {
            return Err("clamp tool image payload must not be empty".into());
        }
        let mut guard = self
            .tool_images
            .write()
            .map_err(|_| "clamp tool image lock poisoned")?;
        let Some(turn) = guard.as_mut() else {
            return Err("no active Antigravity turn is armed for tool images".into());
        };
        if turn
            .images
            .iter()
            .any(|existing| existing.nonce == attachment.nonce)
        {
            return Err(format!(
                "duplicate Antigravity tool image nonce `{}`",
                attachment.nonce
            )
            .into());
        }
        let new_total = turn
            .total_base64_bytes
            .saturating_add(attachment.data_base64.len());
        if new_total > MAX_STAGED_TOOL_IMAGE_BYTES {
            return Err(format!(
                "staged Antigravity tool images ({new_total} base64 bytes) exceed {MAX_STAGED_TOOL_IMAGE_BYTES}-byte limit"
            )
            .into());
        }
        turn.total_base64_bytes = new_total;
        turn.images.push(attachment);
        Ok(())
    }

    pub(super) fn disarm_tool_images(&self) -> Result<usize, BoxError> {
        let mut guard = self
            .tool_images
            .write()
            .map_err(|_| "clamp tool image lock poisoned")?;
        Ok(guard.take().map_or(0, |turn| turn.spliced_requests))
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
            .instructions
            .read()
            .map_err(|_| "clamp system prompt lock poisoned")?
            .clone();
        let active_audio = self
            .audio
            .read()
            .map_err(|_| "clamp audio attachment lock poisoned")?
            .clone();
        let active_tool_images = self
            .tool_images
            .read()
            .map_err(|_| "clamp tool image lock poisoned")?
            .clone();
        let (mut parts, body) = req.into_parts();
        let bytes = Limited::new(body, MAX_GENERATION_REQUEST_BYTES)
            .collect()
            .await?
            .to_bytes();
        let mut payload: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "clamp generation request is not valid JSON")?;
        let request_model = match &envelope {
            GenerationEnvelope::CloudCode => payload
                .get("model")
                .and_then(Value::as_str)
                .map(normalize_model_name)
                .map(str::to_string),
            GenerationEnvelope::Gemini { model } => Some(normalize_model_name(model).to_string()),
        };
        let request = match envelope {
            GenerationEnvelope::CloudCode => payload.get_mut("request"),
            GenerationEnvelope::Gemini { .. } => Some(&mut payload),
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
        let spliced_audio = if let Some(turn) = active_audio.as_ref() {
            splice_authorized_audio(request, request_model.as_deref(), turn)?
        } else {
            false
        };
        let spliced_images = if let Some(turn) = active_tool_images.as_ref()
            && !turn.images.is_empty()
        {
            splice_authorized_tool_images(request, request_model.as_deref(), turn)?
        } else {
            false
        };
        let bytes = serde_json::to_vec(&payload)?;
        if (spliced_audio || spliced_images) && bytes.len() > MAX_GENERATION_REQUEST_BYTES {
            return Err(format!(
                "clamp generation request with attached media ({} bytes) exceeds {MAX_GENERATION_REQUEST_BYTES}-byte limit",
                bytes.len()
            )
            .into());
        }
        if spliced_audio {
            let active_generation = active_audio.as_ref().map(|turn| turn.generation_id);
            let mut guard = self
                .audio
                .write()
                .map_err(|_| "clamp audio attachment lock poisoned")?;
            if let Some(turn) = guard.as_mut()
                && Some(turn.generation_id) == active_generation
            {
                turn.spliced_requests = turn.spliced_requests.saturating_add(1);
            } else {
                return Err(
                    "clamp audio attachment authorization expired before request was sent".into(),
                );
            }
        }
        if spliced_images {
            let active_generation = active_tool_images.as_ref().map(|turn| turn.generation_id);
            let mut guard = self
                .tool_images
                .write()
                .map_err(|_| "clamp tool image lock poisoned")?;
            if let Some(turn) = guard.as_mut()
                && Some(turn.generation_id) == active_generation
            {
                turn.spliced_requests = turn.spliced_requests.saturating_add(1);
            } else {
                return Err(
                    "clamp tool image authorization expired before request was sent".into(),
                );
            }
        }
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
    Gemini { model: String },
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
                return Ok(Some(GenerationEnvelope::Gemini {
                    model: model.to_string(),
                }));
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

fn normalize_model_name(model: &str) -> &str {
    let model = model.trim();
    model.strip_prefix("models/").unwrap_or(model)
}

/// Splice authorized turn audio into the unique matching `role: "user"` content
/// entry of a main agentic generation request.
///
/// Returns `Ok(true)` when all authorized attachments were spliced into this
/// request, `Ok(false)` when the request is not an authorized target (different
/// model, auxiliary call without Chaos MCP tools, or turn prompt not present),
/// and `Err` when the target user part is ambiguous or malformed.
fn splice_authorized_audio(
    request: &mut serde_json::Map<String, Value>,
    request_model: Option<&str>,
    turn: &ActiveAudioTurn,
) -> Result<bool, BoxError> {
    if request_model != Some(turn.authorized_model.as_str()) {
        return Ok(false);
    }
    // Main clamped agentic turns carry Chaos MCP tool declarations; auxiliary
    // calls (for example title or summary generation) do not. Even if an
    // auxiliary call runs on the same model and copies user text including its
    // nonce, never attach user audio to it.
    let has_tools = request
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    if !has_tools {
        return Ok(false);
    }

    let Some(contents) = request.get_mut("contents").and_then(Value::as_array_mut) else {
        return Err("clamp generation request is missing contents".into());
    };

    let mut targets: Vec<(usize, usize)> = Vec::with_capacity(turn.attachments.len());
    for (attachment_idx, attachment) in turn.attachments.iter().enumerate() {
        let marker = format!("id {}", attachment.nonce);
        let mut matched_content_indices = Vec::new();
        for (content_idx, content) in contents.iter().enumerate() {
            if content.get("role").and_then(Value::as_str) != Some("user") {
                continue;
            }
            let Some(parts) = content.get("parts").and_then(Value::as_array) else {
                continue;
            };
            let has_marker = parts.iter().any(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains(&marker))
            });
            if has_marker {
                matched_content_indices.push(content_idx);
            }
        }
        match matched_content_indices.as_slice() {
            [] => return Ok(false),
            [content_idx] => targets.push((*content_idx, attachment_idx)),
            _ => {
                return Err(format!(
                    "ambiguous Antigravity user content for audio attachment id {}",
                    attachment.nonce
                )
                .into());
            }
        }
    }

    for (content_idx, attachment_idx) in targets {
        let attachment = &turn.attachments[attachment_idx];
        let parts = contents[content_idx]
            .get_mut("parts")
            .and_then(Value::as_array_mut)
            .ok_or("matched Antigravity user content is missing parts array")?;
        parts.push(json!({
            "inlineData": {
                "mimeType": attachment.mime_type,
                "data": attachment.data_base64.as_ref(),
            }
        }));
    }

    Ok(true)
}

/// Format the placeholder marker returned over the clamp MCP bridge in place of
/// an offloaded image block.
pub fn antigravity_tool_image_marker(nonce: &str) -> String {
    format!("[chaos-image id {nonce}]")
}

fn json_value_contains_str(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(text) => text.contains(needle),
        Value::Array(items) => items
            .iter()
            .any(|item| json_value_contains_str(item, needle)),
        Value::Object(map) => map
            .values()
            .any(|item| json_value_contains_str(item, needle)),
        _ => false,
    }
}

/// Splice staged tool-returned images into their matching `role: "user"`
/// `functionResponse` parts on a main agentic generation request.
fn splice_authorized_tool_images(
    request: &mut serde_json::Map<String, Value>,
    request_model: Option<&str>,
    turn: &ActiveToolImageTurn,
) -> Result<bool, BoxError> {
    if request_model != Some(turn.authorized_model.as_str()) {
        return Ok(false);
    }
    let has_tools = request
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    if !has_tools {
        return Ok(false);
    }

    let Some(contents) = request.get_mut("contents").and_then(Value::as_array_mut) else {
        return Err("clamp generation request is missing contents".into());
    };

    let mut targets: Vec<(usize, usize, usize)> = Vec::new();
    for (image_idx, image) in turn.images.iter().enumerate() {
        let marker = antigravity_tool_image_marker(&image.nonce);
        let mut matched_parts = Vec::new();
        for (content_idx, content) in contents.iter().enumerate() {
            // Antigravity emits `functionResponse` entries under `role: "model"`,
            // while standard Gemini API uses `role: "user"` or `role: "function"`.
            // Matching strictly on `part.functionResponse` with `name == "call_mcp_tool"`
            // still prevents ordinary user/model text from selecting staged media.
            if !matches!(
                content.get("role").and_then(Value::as_str),
                Some("user" | "model" | "function")
            ) {
                continue;
            }
            let Some(parts) = content.get("parts").and_then(Value::as_array) else {
                continue;
            };
            for (part_idx, part) in parts.iter().enumerate() {
                if let Some(function_response) = part.get("functionResponse")
                    && function_response.is_object()
                    && function_response.get("name").and_then(Value::as_str)
                        == Some("call_mcp_tool")
                    && json_value_contains_str(function_response, &marker)
                {
                    matched_parts.push((content_idx, part_idx));
                }
            }
        }
        match matched_parts.as_slice() {
            [] => continue,
            [(content_idx, part_idx)] => targets.push((*content_idx, *part_idx, image_idx)),
            _ => {
                return Err(format!(
                    "ambiguous Antigravity functionResponse for tool image id {}",
                    image.nonce
                )
                .into());
            }
        }
    }

    if targets.is_empty() {
        return Ok(false);
    }

    for (content_idx, part_idx, image_idx) in targets {
        let image = &turn.images[image_idx];
        let function_response = contents[content_idx]
            .get_mut("parts")
            .and_then(Value::as_array_mut)
            .and_then(|parts| parts.get_mut(part_idx))
            .and_then(|part| part.get_mut("functionResponse"))
            .and_then(Value::as_object_mut)
            .ok_or("matched Antigravity functionResponse is malformed")?;
        let parts = function_response
            .entry("parts")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("matched Antigravity functionResponse parts field is not an array")?;
        parts.push(json!({
            "inlineData": {
                "mimeType": image.mime_type,
                "data": image.data_base64.as_ref(),
            }
        }));
    }

    Ok(true)
}

#[cfg(test)]
mod tests;
