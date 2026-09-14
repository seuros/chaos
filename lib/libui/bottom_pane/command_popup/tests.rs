use super::*;
use pretty_assertions::assert_eq;

pub(crate) fn command_popup_suite() {
    model_is_first_suggestion_for_mo();
    filtered_commands_keep_presentation_order_for_prefix();
    prompt_discovery_lists_custom_prompts();
    prompt_name_collision_with_builtin_is_ignored();
    prompt_description_uses_frontmatter_metadata();
    prompt_description_falls_back_when_missing();
    prefix_filter_limits_matches_for_ac();
    quit_hidden_in_empty_filter_but_shown_for_prefix();
    collab_command_hidden_when_collaboration_modes_disabled();
    collab_command_visible_when_collaboration_modes_enabled();
    plan_command_visible_when_collaboration_modes_enabled();
}

fn model_is_first_suggestion_for_mo() {
    let mut popup = CommandPopup::new(Vec::new(), CommandPopupFlags::default());
    popup.on_composer_text_change("/mo".to_string());
    let matches = popup.filtered_items();
    match matches.first() {
        Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), "model"),
        Some(CommandItem::UserPrompt(_)) => {
            panic!("unexpected prompt ranked before '/model' for '/mo'")
        }
        None => panic!("expected at least one match for '/mo'"),
    }
}

fn filtered_commands_keep_presentation_order_for_prefix() {
    let mut popup = CommandPopup::new(Vec::new(), CommandPopupFlags::default());
    popup.on_composer_text_change("/m".to_string());

    let cmds: Vec<&str> = popup
        .filtered_items()
        .into_iter()
        .filter_map(|item| match item {
            CommandItem::Builtin(cmd) => Some(cmd.command()),
            CommandItem::UserPrompt(_) => None,
        })
        .collect();
    assert_eq!(cmds, vec!["model", "mention", "mcp", "mcp-add"]);
}

fn prompt_discovery_lists_custom_prompts() {
    let prompts = vec![
        CustomPrompt {
            name: "foo".to_string(),
            path: "/tmp/foo.md".to_string().into(),
            content: "hello from foo".to_string(),
            description: None,
            argument_hint: None,
        },
        CustomPrompt {
            name: "bar".to_string(),
            path: "/tmp/bar.md".to_string().into(),
            content: "hello from bar".to_string(),
            description: None,
            argument_hint: None,
        },
    ];
    let popup = CommandPopup::new(prompts, CommandPopupFlags::default());
    let items = popup.filtered_items();
    let mut prompt_names: Vec<String> = items
        .into_iter()
        .filter_map(|it| match it {
            CommandItem::UserPrompt(i) => popup.prompt(i).map(|p| p.name.clone()),
            _ => None,
        })
        .collect();
    prompt_names.sort();
    assert_eq!(prompt_names, vec!["bar".to_string(), "foo".to_string()]);
}

fn prompt_name_collision_with_builtin_is_ignored() {
    // Create a prompt named like a builtin (e.g. "model").
    let popup = CommandPopup::new(
        vec![CustomPrompt {
            name: "model".to_string(),
            path: "/tmp/model.md".to_string().into(),
            content: "should be ignored".to_string(),
            description: None,
            argument_hint: None,
        }],
        CommandPopupFlags::default(),
    );
    let items = popup.filtered_items();
    let has_collision_prompt = items.into_iter().any(|it| match it {
        CommandItem::UserPrompt(i) => popup.prompt(i).is_some_and(|p| p.name == "model"),
        _ => false,
    });
    assert!(
        !has_collision_prompt,
        "prompt with builtin name should be ignored"
    );
}

fn prompt_description_uses_frontmatter_metadata() {
    let popup = CommandPopup::new(
        vec![CustomPrompt {
            name: "draftpr".to_string(),
            path: "/tmp/draftpr.md".to_string().into(),
            content: "body".to_string(),
            description: Some("Create feature branch, commit and open draft PR.".to_string()),
            argument_hint: None,
        }],
        CommandPopupFlags::default(),
    );
    let rows = popup.rows_from_matches(vec![(CommandItem::UserPrompt(0), None)]);
    let description = rows.first().and_then(|row| row.description.as_deref());
    assert_eq!(
        description,
        Some("Create feature branch, commit and open draft PR.")
    );
}

fn prompt_description_falls_back_when_missing() {
    let popup = CommandPopup::new(
        vec![CustomPrompt {
            name: "foo".to_string(),
            path: "/tmp/foo.md".to_string().into(),
            content: "body".to_string(),
            description: None,
            argument_hint: None,
        }],
        CommandPopupFlags::default(),
    );
    let rows = popup.rows_from_matches(vec![(CommandItem::UserPrompt(0), None)]);
    let description = rows.first().and_then(|row| row.description.as_deref());
    assert_eq!(description, Some("send saved prompt"));
}

fn prefix_filter_limits_matches_for_ac() {
    let mut popup = CommandPopup::new(Vec::new(), CommandPopupFlags::default());
    popup.on_composer_text_change("/ac".to_string());

    let cmds: Vec<&str> = popup
        .filtered_items()
        .into_iter()
        .filter_map(|item| match item {
            CommandItem::Builtin(cmd) => Some(cmd.command()),
            CommandItem::UserPrompt(_) => None,
        })
        .collect();
    assert!(
        !cmds.contains(&"compact"),
        "expected prefix search for '/ac' to exclude 'compact', got {cmds:?}"
    );
}

fn quit_hidden_in_empty_filter_but_shown_for_prefix() {
    let mut popup = CommandPopup::new(Vec::new(), CommandPopupFlags::default());
    popup.on_composer_text_change("/".to_string());
    let items = popup.filtered_items();
    assert!(!items.contains(&CommandItem::Builtin(SlashCommand::Quit)));

    popup.on_composer_text_change("/qu".to_string());
    let items = popup.filtered_items();
    assert!(items.contains(&CommandItem::Builtin(SlashCommand::Quit)));
}

fn collab_command_hidden_when_collaboration_modes_disabled() {
    let mut popup = CommandPopup::new(Vec::new(), CommandPopupFlags::default());
    popup.on_composer_text_change("/".to_string());

    let cmds: Vec<&str> = popup
        .filtered_items()
        .into_iter()
        .filter_map(|item| match item {
            CommandItem::Builtin(cmd) => Some(cmd.command()),
            CommandItem::UserPrompt(_) => None,
        })
        .collect();
    assert!(
        !cmds.contains(&"collab"),
        "expected '/collab' to be hidden when collaboration modes are disabled, got {cmds:?}"
    );
    assert!(
        !cmds.contains(&"plan"),
        "expected '/plan' to be hidden when collaboration modes are disabled, got {cmds:?}"
    );
}

fn collab_command_visible_when_collaboration_modes_enabled() {
    let mut popup = CommandPopup::new(
        Vec::new(),
        CommandPopupFlags {
            collaboration_modes_enabled: true,
            login_required: false,
        },
    );
    popup.on_composer_text_change("/collab".to_string());

    match popup.selected_item() {
        Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), "collab"),
        other => panic!("expected collab to be selected for exact match, got {other:?}"),
    }
}

fn plan_command_visible_when_collaboration_modes_enabled() {
    let mut popup = CommandPopup::new(
        Vec::new(),
        CommandPopupFlags {
            collaboration_modes_enabled: true,
            login_required: false,
        },
    );
    popup.on_composer_text_change("/plan".to_string());

    match popup.selected_item() {
        Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), "plan"),
        other => panic!("expected plan to be selected for exact match, got {other:?}"),
    }
}
