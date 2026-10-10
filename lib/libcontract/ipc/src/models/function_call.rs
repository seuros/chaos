use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::ser::Serializer;

use crate::mcp::CallToolResult;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail {
    Auto,
    Low,
    High,
    Original,
}

/// Responses API compatible content items that can be returned by a tool call.
/// This is a subset of ContentItem with the types we support as function call outputs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FunctionCallOutputContentItem {
    // Do not rename, these are serialized and used directly in the responses API.
    InputText {
        text: String,
    },
    // Do not rename, these are serialized and used directly in the responses API.
    InputImage {
        image_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
    },
}

/// Converts structured function-call output content into plain text for
/// human-readable surfaces.
///
/// This conversion is intentionally lossy:
/// - only `input_text` items are included
/// - image items are ignored
///
/// We use this helper where callers still need a string representation (for
/// example telemetry previews or legacy string-only output paths) while keeping
/// the original multimodal `content_items` as the authoritative payload sent to
/// the model.
pub fn function_call_output_content_items_to_text(
    content_items: &[FunctionCallOutputContentItem],
) -> Option<String> {
    let mut text_segments = content_items.iter().filter_map(|item| match item {
        FunctionCallOutputContentItem::InputText { text } if !text.trim().is_empty() => {
            Some(text.as_str())
        }
        FunctionCallOutputContentItem::InputText { .. }
        | FunctionCallOutputContentItem::InputImage { .. } => None,
    });
    let first = text_segments.next()?;
    let capacity = first.len()
        + text_segments
            .clone()
            .map(|segment| segment.len() + 1)
            .sum::<usize>();
    let mut text = String::with_capacity(capacity);
    text.push_str(first);
    for segment in text_segments {
        text.push('\n');
        text.push_str(segment);
    }
    Some(text)
}

impl From<crate::dynamic_tools::DynamicToolCallOutputContentItem>
    for FunctionCallOutputContentItem
{
    fn from(item: crate::dynamic_tools::DynamicToolCallOutputContentItem) -> Self {
        match item {
            crate::dynamic_tools::DynamicToolCallOutputContentItem::InputText { text } => {
                Self::InputText { text }
            }
            crate::dynamic_tools::DynamicToolCallOutputContentItem::InputImage { image_url } => {
                Self::InputImage {
                    image_url,
                    detail: None,
                }
            }
        }
    }
}

/// The payload we send back to OpenAI when reporting a tool call result.
///
/// `body` serializes directly as the wire value for `function_call_output.output`.
/// `success` remains internal metadata for downstream handling.
#[derive(Debug, Default, Clone, PartialEq, JsonSchema)]
pub struct FunctionCallOutputPayload {
    pub body: FunctionCallOutputBody,
    pub success: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(untagged)]
pub enum FunctionCallOutputBody {
    Text(String),
    ContentItems(Vec<FunctionCallOutputContentItem>),
}

impl FunctionCallOutputBody {
    /// Best-effort conversion of a function-call output body to plain text for
    /// human-readable surfaces.
    ///
    /// This conversion is intentionally lossy when the body contains content
    /// items: image entries are dropped and text entries are joined with
    /// newlines.
    pub fn to_text(&self) -> Option<String> {
        match self {
            Self::Text(content) => Some(content.clone()),
            Self::ContentItems(items) => function_call_output_content_items_to_text(items),
        }
    }

    /// Consume an output for a text-only destination without copying its owned
    /// text buffer. Images and blank text retain `to_text`'s lossy semantics.
    pub fn into_text(self) -> Option<String> {
        match self {
            Self::Text(content) => Some(content),
            Self::ContentItems(items) => {
                let capacity = items
                    .iter()
                    .filter_map(|item| match item {
                        FunctionCallOutputContentItem::InputText { text }
                            if !text.trim().is_empty() =>
                        {
                            Some(text.len() + 1)
                        }
                        _ => None,
                    })
                    .sum::<usize>()
                    .saturating_sub(1);
                let mut segments = items.into_iter().filter_map(|item| match item {
                    FunctionCallOutputContentItem::InputText { text }
                        if !text.trim().is_empty() =>
                    {
                        Some(text)
                    }
                    _ => None,
                });
                let mut text = segments.next()?;
                text.reserve(capacity - text.len());
                for segment in segments {
                    text.push('\n');
                    text.push_str(&segment);
                }
                Some(text)
            }
        }
    }
}

impl Default for FunctionCallOutputBody {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

impl FunctionCallOutputPayload {
    pub fn from_text(content: String) -> Self {
        Self {
            body: FunctionCallOutputBody::Text(content),
            success: None,
        }
    }

    pub fn from_content_items(content_items: Vec<FunctionCallOutputContentItem>) -> Self {
        Self {
            body: FunctionCallOutputBody::ContentItems(content_items),
            success: None,
        }
    }

    pub fn text_content(&self) -> Option<&str> {
        match &self.body {
            FunctionCallOutputBody::Text(content) => Some(content),
            FunctionCallOutputBody::ContentItems(_) => None,
        }
    }

    pub fn text_content_mut(&mut self) -> Option<&mut String> {
        match &mut self.body {
            FunctionCallOutputBody::Text(content) => Some(content),
            FunctionCallOutputBody::ContentItems(_) => None,
        }
    }

    pub fn content_items(&self) -> Option<&[FunctionCallOutputContentItem]> {
        match &self.body {
            FunctionCallOutputBody::Text(_) => None,
            FunctionCallOutputBody::ContentItems(items) => Some(items),
        }
    }

    pub fn content_items_mut(&mut self) -> Option<&mut Vec<FunctionCallOutputContentItem>> {
        match &mut self.body {
            FunctionCallOutputBody::Text(_) => None,
            FunctionCallOutputBody::ContentItems(items) => Some(items),
        }
    }
}

// `function_call_output.output` is encoded as either:
//   - an array of structured content items
//   - a plain string
impl Serialize for FunctionCallOutputPayload {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.body {
            FunctionCallOutputBody::Text(content) => serializer.serialize_str(content),
            FunctionCallOutputBody::ContentItems(items) => items.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for FunctionCallOutputPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let body = FunctionCallOutputBody::deserialize(deserializer)?;
        Ok(FunctionCallOutputPayload {
            body,
            success: None,
        })
    }
}

impl CallToolResult {
    pub fn from_result(result: Result<Self, String>) -> Self {
        match result {
            Ok(result) => result,
            Err(error) => Self::from_error_text(error),
        }
    }

    pub fn from_error_text(text: String) -> Self {
        Self {
            content: vec![serde_json::json!({
                "type": "text",
                "text": text,
            })],
            structured_content: None,
            is_error: Some(true),
            meta: None,
        }
    }

    pub fn success(&self) -> bool {
        self.is_error != Some(true)
    }

    pub fn as_function_call_output_payload(&self) -> FunctionCallOutputPayload {
        if let Some(structured_content) = &self.structured_content
            && !structured_content.is_null()
        {
            match serde_json::to_string(structured_content) {
                Ok(serialized_structured_content) => {
                    return FunctionCallOutputPayload {
                        body: FunctionCallOutputBody::Text(serialized_structured_content),
                        success: Some(self.success()),
                    };
                }
                Err(err) => {
                    return FunctionCallOutputPayload {
                        body: FunctionCallOutputBody::Text(err.to_string()),
                        success: Some(false),
                    };
                }
            }
        }

        if let Some(content_items) = convert_mcp_content_to_items(&self.content) {
            return FunctionCallOutputPayload {
                body: FunctionCallOutputBody::ContentItems(content_items),
                success: Some(self.success()),
            };
        }

        let serialized_content = match serde_json::to_string(&self.content) {
            Ok(serialized_content) => serialized_content,
            Err(err) => {
                return FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text(err.to_string()),
                    success: Some(false),
                };
            }
        };

        FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(serialized_content),
            success: Some(self.success()),
        }
    }

    pub fn into_function_call_output_payload(self) -> FunctionCallOutputPayload {
        if self
            .structured_content
            .as_ref()
            .is_some_and(|value| !value.is_null())
            || !self.content.iter().any(is_mcp_image)
        {
            return self.as_function_call_output_payload();
        }
        let success = Some(self.success());
        let items = self
            .content
            .into_iter()
            .map(|mut content| match McpContent::deserialize(&content) {
                Ok(McpContent::Text { .. }) => {
                    let serde_json::Value::String(text) = content["text"].take() else {
                        unreachable!("validated MCP text")
                    };
                    FunctionCallOutputContentItem::InputText { text }
                }
                Ok(McpContent::Image { mime_type, .. }) => {
                    let prefix = if content["data"]
                        .as_str()
                        .is_some_and(|data| data.starts_with("data:"))
                    {
                        None
                    } else {
                        Some(format!(
                            "data:{};base64,",
                            mime_type.unwrap_or("application/octet-stream")
                        ))
                    };
                    let serde_json::Value::String(mut image_url) = content["data"].take() else {
                        unreachable!("validated MCP image")
                    };
                    if let Some(prefix) = prefix {
                        image_url.insert_str(0, &prefix);
                    }
                    FunctionCallOutputContentItem::InputImage {
                        image_url,
                        detail: None,
                    }
                }
                Ok(McpContent::Unknown) | Err(_) => FunctionCallOutputContentItem::InputText {
                    text: serde_json::to_string(&content)
                        .unwrap_or_else(|_| "<content>".to_string()),
                },
            })
            .collect();
        FunctionCallOutputPayload {
            body: FunctionCallOutputBody::ContentItems(items),
            success,
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(tag = "type")]
enum McpContent<'a> {
    #[serde(rename = "text")]
    Text { text: &'a str },
    #[serde(rename = "image")]
    Image {
        data: &'a str,
        #[serde(rename = "mimeType", alias = "mime_type")]
        mime_type: Option<&'a str>,
    },
    #[serde(other)]
    Unknown,
}

fn is_mcp_image(content: &serde_json::Value) -> bool {
    matches!(
        McpContent::deserialize(content),
        Ok(McpContent::Image { .. })
    )
}

pub(super) fn convert_mcp_content_to_items(
    contents: &[serde_json::Value],
) -> Option<Vec<FunctionCallOutputContentItem>> {
    // Text-only results go straight to their wire representation. In particular,
    // do not allocate and then discard a second copy of every text block.
    if !contents.iter().any(is_mcp_image) {
        return None;
    }
    let mut items = Vec::with_capacity(contents.len());

    for content in contents {
        let item = match McpContent::deserialize(content) {
            Ok(McpContent::Text { text }) => FunctionCallOutputContentItem::InputText {
                text: text.to_owned(),
            },
            Ok(McpContent::Image { data, mime_type }) => {
                let image_url = if data.starts_with("data:") {
                    data.to_owned()
                } else {
                    let mime_type = mime_type.unwrap_or("application/octet-stream");
                    format!("data:{mime_type};base64,{data}")
                };
                FunctionCallOutputContentItem::InputImage {
                    image_url,
                    detail: None,
                }
            }
            Ok(McpContent::Unknown) | Err(_) => FunctionCallOutputContentItem::InputText {
                text: serde_json::to_string(content).unwrap_or_else(|_| "<content>".to_string()),
            },
        };
        items.push(item);
    }

    Some(items)
}

// Implement Display so callers can treat the payload like a plain string when logging or doing
// trivial substring checks in tests (existing tests call `.contains()` on the output). For
// `ContentItems`, Display emits a JSON representation.

impl std::fmt::Display for FunctionCallOutputPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.body {
            FunctionCallOutputBody::Text(content) => f.write_str(content),
            FunctionCallOutputBody::ContentItems(items) => {
                let content = serde_json::to_string(items).unwrap_or_default();
                f.write_str(content.as_str())
            }
        }
    }
}
