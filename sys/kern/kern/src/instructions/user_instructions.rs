use serde::Deserialize;
use serde::Serialize;

use chaos_ipc::models::ResponseItem;

use crate::contextual_user_message::AGENTS_MD_FRAGMENT;

pub const USER_INSTRUCTIONS_PREFIX: &str = "# AGENTS.md instructions for ";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename = "user_instructions", rename_all = "snake_case")]
pub(crate) struct UserInstructions {
    pub directory: String,
    pub text: String,
}

impl UserInstructions {
    pub(crate) fn serialize_to_text(&self) -> String {
        format!(
            "{prefix}{directory}\n\n<INSTRUCTIONS>\n{contents}\n{suffix}",
            prefix = AGENTS_MD_FRAGMENT.start_marker(),
            directory = self.directory,
            contents = self.text,
            suffix = AGENTS_MD_FRAGMENT.end_marker(),
        )
    }
}

impl From<UserInstructions> for ResponseItem {
    fn from(ui: UserInstructions) -> Self {
        AGENTS_MD_FRAGMENT.into_message(ui.serialize_to_text())
    }
}

#[cfg(test)]
#[path = "user_instructions_tests.rs"]
mod tests;
