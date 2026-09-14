use super::*;
use pretty_assertions::assert_eq;

pub(crate) fn prompt_args_suite() {
    expand_arguments_basic();
    quoted_values_ok();
    invalid_arg_token_reports_error();
    missing_required_args_reports_error();
    escaped_placeholder_is_ignored();
    escaped_placeholder_remains_literal();
    positional_args_treat_placeholder_with_spaces_as_single_token();
    extract_positional_args_shifts_element_offsets_into_args_str();
    key_value_args_treat_placeholder_with_spaces_as_single_token();
    positional_args_allow_placeholder_inside_quotes();
    key_value_args_allow_placeholder_inside_quotes();
}

fn expand_arguments_basic() {
    let prompts = vec![CustomPrompt {
        name: "my-prompt".to_string(),
        path: "/tmp/my-prompt.md".to_string().into(),
        content: "Review $USER changes on $BRANCH".to_string(),
        description: None,
        argument_hint: None,
    }];

    let out =
        expand_custom_prompt("/prompts:my-prompt USER=Alice BRANCH=main", &[], &prompts).unwrap();
    assert_eq!(
        out,
        Some(PromptExpansion {
            text: "Review Alice changes on main".to_string(),
            text_elements: Vec::new(),
        })
    );
}

fn quoted_values_ok() {
    let prompts = vec![CustomPrompt {
        name: "my-prompt".to_string(),
        path: "/tmp/my-prompt.md".to_string().into(),
        content: "Pair $USER with $BRANCH".to_string(),
        description: None,
        argument_hint: None,
    }];

    let out = expand_custom_prompt(
        "/prompts:my-prompt USER=\"Alice Smith\" BRANCH=dev-main",
        &[],
        &prompts,
    )
    .unwrap();
    assert_eq!(
        out,
        Some(PromptExpansion {
            text: "Pair Alice Smith with dev-main".to_string(),
            text_elements: Vec::new(),
        })
    );
}

fn invalid_arg_token_reports_error() {
    let prompts = vec![CustomPrompt {
        name: "my-prompt".to_string(),
        path: "/tmp/my-prompt.md".to_string().into(),
        content: "Review $USER changes".to_string(),
        description: None,
        argument_hint: None,
    }];
    let err = expand_custom_prompt("/prompts:my-prompt USER=Alice stray", &[], &prompts)
        .unwrap_err()
        .user_message();
    assert!(err.contains("expected key=value"));
}

fn missing_required_args_reports_error() {
    let prompts = vec![CustomPrompt {
        name: "my-prompt".to_string(),
        path: "/tmp/my-prompt.md".to_string().into(),
        content: "Review $USER changes on $BRANCH".to_string(),
        description: None,
        argument_hint: None,
    }];
    let err = expand_custom_prompt("/prompts:my-prompt USER=Alice", &[], &prompts)
        .unwrap_err()
        .user_message();
    assert!(err.to_lowercase().contains("missing required args"));
    assert!(err.contains("BRANCH"));
}

fn escaped_placeholder_is_ignored() {
    assert_eq!(
        prompt_argument_names("literal $$USER"),
        Vec::<String>::new()
    );
    assert_eq!(
        prompt_argument_names("literal $$USER and $REAL"),
        vec!["REAL".to_string()]
    );
}

fn escaped_placeholder_remains_literal() {
    let prompts = vec![CustomPrompt {
        name: "my-prompt".to_string(),
        path: "/tmp/my-prompt.md".to_string().into(),
        content: "literal $$USER".to_string(),
        description: None,
        argument_hint: None,
    }];

    let out = expand_custom_prompt("/prompts:my-prompt", &[], &prompts).unwrap();
    assert_eq!(
        out,
        Some(PromptExpansion {
            text: "literal $$USER".to_string(),
            text_elements: Vec::new(),
        })
    );
}

fn positional_args_treat_placeholder_with_spaces_as_single_token() {
    let placeholder = "[Image #1]";
    let rest = format!("alpha {placeholder} beta");
    let start = rest.find(placeholder).expect("placeholder");
    let end = start + placeholder.len();
    let text_elements = vec![TextElement::new(
        ByteRange { start, end },
        Some(placeholder.to_string()),
    )];

    let args = parse_positional_args(&rest, &text_elements);
    assert_eq!(
        args,
        vec![
            PromptArg {
                text: "alpha".to_string(),
                text_elements: Vec::new(),
            },
            PromptArg {
                text: placeholder.to_string(),
                text_elements: vec![TextElement::new(
                    ByteRange {
                        start: 0,
                        end: placeholder.len(),
                    },
                    Some(placeholder.to_string()),
                )],
            },
            PromptArg {
                text: "beta".to_string(),
                text_elements: Vec::new(),
            }
        ]
    );
}

fn extract_positional_args_shifts_element_offsets_into_args_str() {
    let placeholder = "[Image #1]";
    let line = format!("  /{PROMPTS_CMD_PREFIX}:my-prompt  alpha {placeholder} beta   ");
    let start = line.find(placeholder).expect("placeholder");
    let end = start + placeholder.len();
    let text_elements = vec![TextElement::new(
        ByteRange { start, end },
        Some(placeholder.to_string()),
    )];

    let args = extract_positional_args_for_prompt_line(&line, "my-prompt", &text_elements);
    assert_eq!(
        args,
        vec![
            PromptArg {
                text: "alpha".to_string(),
                text_elements: Vec::new(),
            },
            PromptArg {
                text: placeholder.to_string(),
                text_elements: vec![TextElement::new(
                    ByteRange {
                        start: 0,
                        end: placeholder.len(),
                    },
                    Some(placeholder.to_string()),
                )],
            },
            PromptArg {
                text: "beta".to_string(),
                text_elements: Vec::new(),
            }
        ]
    );
}

fn key_value_args_treat_placeholder_with_spaces_as_single_token() {
    let placeholder = "[Image #1]";
    let rest = format!("IMG={placeholder} NOTE=hello");
    let start = rest.find(placeholder).expect("placeholder");
    let end = start + placeholder.len();
    let text_elements = vec![TextElement::new(
        ByteRange { start, end },
        Some(placeholder.to_string()),
    )];

    let args = parse_prompt_inputs(&rest, &text_elements).expect("inputs");
    assert_eq!(
        args.get("IMG"),
        Some(&PromptArg {
            text: placeholder.to_string(),
            text_elements: vec![TextElement::new(
                ByteRange {
                    start: 0,
                    end: placeholder.len(),
                },
                Some(placeholder.to_string()),
            )],
        })
    );
    assert_eq!(
        args.get("NOTE"),
        Some(&PromptArg {
            text: "hello".to_string(),
            text_elements: Vec::new(),
        })
    );
}

fn positional_args_allow_placeholder_inside_quotes() {
    let placeholder = "[Image #1]";
    let rest = format!("alpha \"see {placeholder} here\" beta");
    let start = rest.find(placeholder).expect("placeholder");
    let end = start + placeholder.len();
    let text_elements = vec![TextElement::new(
        ByteRange { start, end },
        Some(placeholder.to_string()),
    )];

    let args = parse_positional_args(&rest, &text_elements);
    assert_eq!(
        args,
        vec![
            PromptArg {
                text: "alpha".to_string(),
                text_elements: Vec::new(),
            },
            PromptArg {
                text: format!("see {placeholder} here"),
                text_elements: vec![TextElement::new(
                    ByteRange {
                        start: "see ".len(),
                        end: "see ".len() + placeholder.len(),
                    },
                    Some(placeholder.to_string()),
                )],
            },
            PromptArg {
                text: "beta".to_string(),
                text_elements: Vec::new(),
            }
        ]
    );
}

fn key_value_args_allow_placeholder_inside_quotes() {
    let placeholder = "[Image #1]";
    let rest = format!("IMG=\"see {placeholder} here\" NOTE=ok");
    let start = rest.find(placeholder).expect("placeholder");
    let end = start + placeholder.len();
    let text_elements = vec![TextElement::new(
        ByteRange { start, end },
        Some(placeholder.to_string()),
    )];

    let args = parse_prompt_inputs(&rest, &text_elements).expect("inputs");
    assert_eq!(
        args.get("IMG"),
        Some(&PromptArg {
            text: format!("see {placeholder} here"),
            text_elements: vec![TextElement::new(
                ByteRange {
                    start: "see ".len(),
                    end: "see ".len() + placeholder.len(),
                },
                Some(placeholder.to_string()),
            )],
        })
    );
    assert_eq!(
        args.get("NOTE"),
        Some(&PromptArg {
            text: "ok".to_string(),
            text_elements: Vec::new(),
        })
    );
}
