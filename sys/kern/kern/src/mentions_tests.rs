use std::collections::HashSet;

use chaos_ipc::user_input::UserInput;
use pretty_assertions::assert_eq;

use super::collect_explicit_app_ids;

fn text_input(text: &str) -> UserInput {
    UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }
}

#[test]
fn collect_explicit_app_ids_from_linked_text_mentions() {
    let input = vec![text_input("use [$calendar](app://calendar)")];

    let app_ids = collect_explicit_app_ids(&input);

    assert_eq!(app_ids, HashSet::from(["calendar".to_string()]));
}

#[test]
fn collect_explicit_app_ids_dedupes_structured_and_linked_mentions() {
    let input = vec![
        text_input("use [$calendar](app://calendar)"),
        UserInput::Mention {
            name: "calendar".to_string(),
            path: "app://calendar".to_string(),
        },
    ];

    let app_ids = collect_explicit_app_ids(&input);

    assert_eq!(app_ids, HashSet::from(["calendar".to_string()]));
}

#[test]
fn collect_explicit_app_ids_ignores_non_app_paths() {
    let input = vec![
        text_input(
            "use [$docs](mcp://docs) and [$skill](skill://team/skill) and [$file](/tmp/file.txt)",
        ),
        UserInput::Mention {
            name: "docs".to_string(),
            path: "mcp://docs".to_string(),
        },
        UserInput::Mention {
            name: "skill".to_string(),
            path: "skill://team/skill".to_string(),
        },
        UserInput::Mention {
            name: "file".to_string(),
            path: "/tmp/file.txt".to_string(),
        },
    ];

    let app_ids = collect_explicit_app_ids(&input);

    assert_eq!(app_ids, HashSet::<String>::new());
}
