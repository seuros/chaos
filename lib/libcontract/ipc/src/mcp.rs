//! Types used when representing Model Context Protocol (MCP) values inside the
//! Chaos protocol.
//!
//! These types support Serde serialization and JSON Schema generation via
//! `schemars` so they can be embedded in Chaos's own protocol structures.
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// ID of a request, which can be either a string or an integer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RequestId {
    String(String),
    Integer(i64),
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestId::String(s) => f.write_str(s),
            RequestId::Integer(i) => i.fmt(f),
        }
    }
}

/// Definition for a tool the client can call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<serde_json::Value>>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

/// A known resource that the server is capable of reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<serde_json::Value>>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

/// A template description for resources available on the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTemplate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<serde_json::Value>,
    pub uri_template: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// The server's response to a tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    pub content: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

// === Adapter helpers ===
//
// Convert MCP JSON payloads into ChaOS IPC types.

fn deserialize_lossy_opt_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<serde_json::Number>::deserialize(deserializer)? {
        Some(number) => {
            if let Some(v) = number.as_i64() {
                Ok(Some(v))
            } else if let Some(v) = number.as_u64() {
                Ok(i64::try_from(v).ok())
            } else {
                Ok(None)
            }
        }
        None => Ok(None),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolSerde {
    name: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "inputSchema", alias = "input_schema")]
    input_schema: serde_json::Value,
    #[serde(default, rename = "outputSchema", alias = "output_schema")]
    output_schema: Option<serde_json::Value>,
    #[serde(default)]
    annotations: Option<serde_json::Value>,
    #[serde(default)]
    icons: Option<Vec<serde_json::Value>>,
    #[serde(rename = "_meta", default)]
    meta: Option<serde_json::Value>,
}

impl From<ToolSerde> for Tool {
    fn from(value: ToolSerde) -> Self {
        let ToolSerde {
            name,
            title,
            description,
            input_schema,
            output_schema,
            annotations,
            icons,
            meta,
        } = value;
        Self {
            name,
            title,
            description,
            input_schema,
            output_schema,
            annotations,
            icons,
            meta,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceSerde {
    #[serde(default)]
    annotations: Option<serde_json::Value>,
    #[serde(default)]
    description: Option<String>,
    #[serde(rename = "mimeType", alias = "mime_type", default)]
    mime_type: Option<String>,
    name: String,
    #[serde(default, deserialize_with = "deserialize_lossy_opt_i64")]
    size: Option<i64>,
    #[serde(default)]
    title: Option<String>,
    uri: String,
    #[serde(default)]
    icons: Option<Vec<serde_json::Value>>,
    #[serde(rename = "_meta", default)]
    meta: Option<serde_json::Value>,
}

impl From<ResourceSerde> for Resource {
    fn from(value: ResourceSerde) -> Self {
        let ResourceSerde {
            annotations,
            description,
            mime_type,
            name,
            size,
            title,
            uri,
            icons,
            meta,
        } = value;
        Self {
            annotations,
            description,
            mime_type,
            name,
            size,
            title,
            uri,
            icons,
            meta,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceTemplateSerde {
    #[serde(default)]
    annotations: Option<serde_json::Value>,
    #[serde(rename = "uriTemplate", alias = "uri_template")]
    uri_template: String,
    name: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(rename = "mimeType", alias = "mime_type", default)]
    mime_type: Option<String>,
}

impl From<ResourceTemplateSerde> for ResourceTemplate {
    fn from(value: ResourceTemplateSerde) -> Self {
        let ResourceTemplateSerde {
            annotations,
            uri_template,
            name,
            title,
            description,
            mime_type,
        } = value;
        Self {
            annotations,
            uri_template,
            name,
            title,
            description,
            mime_type,
        }
    }
}

impl Tool {
    pub fn from_mcp_value(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        Ok(serde_json::from_value::<ToolSerde>(value)?.into())
    }
}

impl Resource {
    pub fn from_mcp_value(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        Ok(serde_json::from_value::<ResourceSerde>(value)?.into())
    }
}

impl ResourceTemplate {
    pub fn from_mcp_value(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        Ok(serde_json::from_value::<ResourceTemplateSerde>(value)?.into())
    }
}

#[cfg(test)]
mod tests;
