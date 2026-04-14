use std::collections::HashSet;

use chaos_ipc::user_input::UserInput;
use regex_lite::Regex;

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
    let Ok(app_link_regex) = Regex::new(r"\[[^\]]*]\(app://([^)]+)\)") else {
        return HashSet::new();
    };

    input
        .iter()
        .flat_map(|item| match item {
            UserInput::Mention { path, .. } => path
                .strip_prefix("app://")
                .map(ToString::to_string)
                .into_iter()
                .collect::<Vec<_>>(),
            UserInput::Text { text, .. } => app_link_regex
                .captures_iter(text)
                .filter_map(|captures| captures.get(1).map(|m| m.as_str().to_string()))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}

#[cfg(test)]
#[path = "mentions_tests.rs"]
mod tests;
