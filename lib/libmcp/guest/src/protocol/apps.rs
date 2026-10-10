//! Client-side MCP Apps discovery and resource consumption.
//!
//! HTML and server metadata are untrusted. This module does not render views,
//! grant browser permissions, or implement the iframe bridge.

use std::borrow::Cow;

use base64::Engine;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

pub use mcp_host::protocol::apps::AppsError;
pub use mcp_host::protocol::apps::EXTENSION_ID;
pub use mcp_host::protocol::apps::MIME_TYPE;
pub use mcp_host::protocol::apps::URI_SCHEME;
pub use mcp_host::protocol::apps::UiCsp;
pub use mcp_host::protocol::apps::UiPermissions;
pub use mcp_host::protocol::apps::UiResourceMeta;
pub use mcp_host::protocol::apps::UiVisibility;
pub use mcp_host::protocol::apps::extension_settings;
pub use mcp_host::protocol::apps::is_ui_uri;

use super::Meta;
use super::ResourceContents;
use super::ResourceInfo;
use crate::GuestError;

/// The tool's `_meta.ui` declaration, including visibility-only declarations.
///
/// Raw metadata remains on [`super::ToolInfo`], including unknown fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolUi {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Vec<UiVisibility>>,
}

impl ToolUi {
    pub fn validate(&self) -> Result<(), AppsError> {
        if let Some(uri) = &self.resource_uri
            && !is_ui_uri(uri)
        {
            return Err(AppsError::InvalidUri(uri.clone()));
        }
        Ok(())
    }

    /// Absent visibility means both model and app; an empty list means neither.
    pub fn is_visible_to(&self, caller: UiVisibility) -> bool {
        self.visibility
            .as_ref()
            .is_none_or(|visibility| visibility.contains(&caller))
    }
}

/// A `ui://` HTML view with validated, effective sandbox metadata.
///
/// Content-level `_meta.ui` replaces listing-level `_meta.ui` in its entirety.
/// Missing policy defaults to deny-all CSP and no browser permissions. The
/// embedding renderer must enforce that policy, not merely display the HTML.
#[derive(Debug, Clone, PartialEq)]
pub struct UiResource {
    /// Original contents, preserving raw `_meta` (including unknown fields).
    pub contents: ResourceContents,
    pub ui: UiResourceMeta,
}

impl UiResource {
    pub fn from_contents(
        contents: ResourceContents,
        listing: Option<&ResourceInfo>,
    ) -> Result<Self, GuestError> {
        if !contents.is_mcp_app() {
            return Err(GuestError::Protocol(format!(
                "MCP Apps contents require a ui:// URI and {MIME_TYPE} MIME type"
            )));
        }
        if listing.is_some_and(|listing| listing.uri != contents.uri()) {
            return Err(GuestError::InvalidParams(
                "MCP Apps listing URI does not match resource contents".to_string(),
            ));
        }
        let ui = match contents.ui()? {
            Some(ui) => ui,
            None => listing
                .map(ResourceInfo::ui)
                .transpose()?
                .flatten()
                .unwrap_or_default(),
        };
        Ok(Self { contents, ui })
    }

    /// Borrow text HTML or decode a base64 blob as UTF-8 HTML.
    pub fn html(&self) -> Result<Cow<'_, str>, GuestError> {
        match &self.contents {
            ResourceContents::Text(contents) => Ok(Cow::Borrowed(&contents.text)),
            ResourceContents::Blob(contents) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&contents.blob)
                    .map_err(|error| {
                        GuestError::Protocol(format!("invalid MCP Apps base64 HTML: {error}"))
                    })?;
                let html = String::from_utf8(bytes).map_err(|error| {
                    GuestError::Protocol(format!("MCP Apps HTML must be UTF-8: {error}"))
                })?;
                Ok(Cow::Owned(html))
            }
        }
    }
}

pub(crate) fn supports_mcp_apps(settings: Option<&Value>) -> bool {
    settings
        .and_then(|settings| settings.get("mimeTypes"))
        .and_then(Value::as_array)
        .is_some_and(|types| types.iter().any(|mime_type| mime_type == MIME_TYPE))
}

pub(crate) fn tool_ui(meta: Option<&Meta>) -> Result<Option<ToolUi>, GuestError> {
    let Some(value) = ui_value(meta)? else {
        return Ok(None);
    };
    let ui = ToolUi::deserialize(value)?;
    ui.validate()
        .map_err(|error| GuestError::Protocol(error.to_string()))?;
    Ok(Some(ui))
}

pub(crate) fn resource_ui(meta: Option<&Meta>) -> Result<Option<UiResourceMeta>, GuestError> {
    let Some(value) = ui_value(meta)? else {
        return Ok(None);
    };
    let ui = UiResourceMeta::deserialize(value)?;
    if let Some(csp) = &ui.csp {
        csp.validate()
            .map_err(|error| GuestError::Protocol(error.to_string()))?;
    }
    Ok(Some(ui))
}

fn ui_value(meta: Option<&Meta>) -> Result<Option<&Value>, GuestError> {
    match meta {
        None => Ok(None),
        Some(Value::Object(meta)) => Ok(meta.get("ui")),
        Some(_) => Err(GuestError::Protocol(
            "MCP Apps _meta must be an object".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests;
