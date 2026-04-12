use chaos_pixbuf::PromptImageMode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

use super::images::image_close_tag_text;
use super::images::image_open_tag_text;
use super::images::local_image_content_items_with_label_number;
use super::permissions::PermissionProfile;
use super::permissions::SandboxPermissions;
use super::response::ContentItem;
use super::response::ResponseInputItem;
use crate::user_input::UserInput;

impl From<Vec<UserInput>> for ResponseInputItem {
    fn from(items: Vec<UserInput>) -> Self {
        let mut image_index = 0;
        Self::Message {
            role: "user".to_string(),
            content: items
                .into_iter()
                .flat_map(|c| match c {
                    UserInput::Text { text, .. } => vec![ContentItem::InputText { text }],
                    UserInput::Image { image_url } => {
                        image_index += 1;
                        vec![
                            ContentItem::InputText {
                                text: image_open_tag_text(),
                            },
                            ContentItem::InputImage { image_url },
                            ContentItem::InputText {
                                text: image_close_tag_text(),
                            },
                        ]
                    }
                    UserInput::LocalImage { path } => {
                        image_index += 1;
                        local_image_content_items_with_label_number(
                            &path,
                            Some(image_index),
                            PromptImageMode::ResizeToFit,
                        )
                    }
                    UserInput::Skill { .. } | UserInput::Mention { .. } => Vec::new(), // Tool bodies are injected later in core
                })
                .collect::<Vec<ContentItem>>(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct SearchToolCallParams {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub limit: Option<usize>,
}

/// If the `name` of a `ResponseItem::FunctionCall` is either `container.exec`
/// or `shell`, the `arguments` field should deserialize to this struct.
#[derive(Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct ShellToolCallParams {
    pub command: Vec<String>,
    pub workdir: Option<String>,

    /// This is the maximum time in milliseconds that the command is allowed to run.
    #[serde(alias = "timeout")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sandbox_permissions: Option<SandboxPermissions>,
    /// Suggests a command prefix to persist for future sessions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub prefix_rule: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub additional_permissions: Option<PermissionProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
}

/// If the `name` of a `ResponseItem::FunctionCall` is `shell_command`, the
/// `arguments` field should deserialize to this struct.
#[derive(Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct ShellCommandToolCallParams {
    pub command: String,
    pub workdir: Option<String>,

    /// Whether to run the shell with login shell semantics
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<bool>,
    /// This is the maximum time in milliseconds that the command is allowed to run.
    #[serde(alias = "timeout")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sandbox_permissions: Option<SandboxPermissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub prefix_rule: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub additional_permissions: Option<PermissionProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
}
