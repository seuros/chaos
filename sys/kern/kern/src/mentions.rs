use std::collections::HashSet;

use chaos_ipc::user_input::UserInput;

pub(crate) struct CollectedToolMentions {
    pub(crate) _plain_names: HashSet<String>,
    pub(crate) _paths: HashSet<String>,
}

#[allow(dead_code)]
pub(crate) fn collect_tool_mentions_from_messages(_messages: &[String]) -> CollectedToolMentions {
    CollectedToolMentions {
        _plain_names: HashSet::new(),
        _paths: HashSet::new(),
    }
}

#[allow(dead_code)]
pub(crate) fn collect_explicit_app_ids(input: &[UserInput]) -> HashSet<String> {
    input
        .iter()
        .filter_map(|item| match item {
            UserInput::Mention { path, .. } => path.strip_prefix("app://").map(ToString::to_string),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
#[path = "mentions_tests.rs"]
mod tests;
