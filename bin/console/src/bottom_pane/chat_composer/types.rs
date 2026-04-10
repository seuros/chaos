use super::*;

/// If the pasted content exceeds this number of characters, replace it with a
/// placeholder in the UI.
pub(super) const LARGE_PASTE_CHAR_THRESHOLD: usize = 1000;

pub(super) fn user_input_too_large_message(actual_chars: usize) -> String {
    format!(
        "Message exceeds the maximum length of {MAX_USER_INPUT_TEXT_CHARS} characters ({actual_chars} provided)."
    )
}

/// Result returned when the user interacts with the text area.
#[derive(Debug, PartialEq)]
pub(crate) enum InputResult {
    Submitted {
        text: String,
        text_elements: Vec<TextElement>,
    },
    Queued {
        text: String,
        text_elements: Vec<TextElement>,
    },
    Command(SlashCommand),
    CommandWithArgs(SlashCommand, String, Vec<TextElement>),
    None,
}

pub(super) enum PromptSelectionMode {
    Completion,
    Submit,
}

pub(super) enum PromptSelectionAction {
    Insert {
        text: String,
        cursor: Option<usize>,
    },
    Submit {
        text: String,
        text_elements: Vec<TextElement>,
    },
}

/// Feature flags for reusing the chat composer in other bottom-pane surfaces.
///
/// The default keeps today's behavior intact. Other call sites can opt out of
/// specific behaviors by constructing a config with those flags set to `false`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChatComposerConfig {
    /// Whether command/file/skill popups are allowed to appear.
    pub(crate) popups_enabled: bool,
    /// Whether `/...` input is parsed and dispatched as slash commands.
    pub(crate) slash_commands_enabled: bool,
    /// Whether pasting a file path can attach local images.
    pub(crate) image_paste_enabled: bool,
}

impl Default for ChatComposerConfig {
    fn default() -> Self {
        Self {
            popups_enabled: true,
            slash_commands_enabled: true,
            image_paste_enabled: true,
        }
    }
}

impl ChatComposerConfig {
    /// A minimal preset for plain-text inputs embedded in other surfaces.
    ///
    /// This disables popups, slash commands, and image-path attachment behavior
    /// so the composer behaves like a simple notes field.
    pub(crate) const fn plain_text() -> Self {
        Self {
            popups_enabled: false,
            slash_commands_enabled: false,
            image_paste_enabled: false,
        }
    }
}
