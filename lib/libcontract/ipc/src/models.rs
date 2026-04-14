mod function_call;
mod images;
mod instructions;
mod permissions;
mod response;
mod shell;
mod tool_params;

pub use function_call::*;
pub use images::*;
pub use instructions::*;
pub use permissions::*;
pub use response::*;
pub use shell::*;
pub use tool_params::*;

#[cfg(test)]
mod tests {
    use super::function_call::convert_mcp_content_to_items;
    use super::instructions::{
        APPROVAL_POLICY_ON_REQUEST_RULE_REQUEST_PERMISSION, MAX_ALLOW_PREFIX_TEXT_BYTES,
        MAX_RENDERED_PREFIXES, TRUNCATED_MARKER, granular_prompt_intro_text,
        request_permissions_tool_prompt_section,
    };
    use super::*;
    use crate::config_types::SandboxMode;
    use crate::mcp::CallToolResult;
    use crate::protocol::ApprovalPolicy;
    use crate::protocol::GranularApprovalConfig;
    use crate::protocol::NetworkAccess;
    use crate::protocol::SandboxPolicy;
    use crate::user_input::UserInput;
    use anyhow::Result;
    use chaos_selinux::Policy;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn sandbox_permissions_helpers_match_documented_semantics() {
        let cases = [
            (SandboxPermissions::UseDefault, false, false, false),
            (SandboxPermissions::RequireEscalated, true, true, false),
            (
                SandboxPermissions::WithAdditionalPermissions,
                false,
                true,
                true,
            ),
        ];

        for (
            sandbox_permissions,
            requires_escalated_permissions,
            requests_sandbox_override,
            uses_additional_permissions,
        ) in cases
        {
            assert_eq!(
                sandbox_permissions.requires_escalated_permissions(),
                requires_escalated_permissions
            );
            assert_eq!(
                sandbox_permissions.requests_sandbox_override(),
                requests_sandbox_override
            );
            assert_eq!(
                sandbox_permissions.uses_additional_permissions(),
                uses_additional_permissions
            );
        }
    }

    #[test]
    fn convert_mcp_content_to_items_preserves_data_urls() {
        let contents = vec![serde_json::json!({
            "type": "image",
            "data": "data:image/png;base64,Zm9v",
            "mimeType": "image/png",
        })];

        let items = convert_mcp_content_to_items(&contents).expect("expected image items");
        assert_eq!(
            items,
            vec![FunctionCallOutputContentItem::InputImage {
                image_url: "data:image/png;base64,Zm9v".to_string(),
                detail: None,
            }]
        );
    }

    #[test]
    fn response_item_parses_image_generation_call() {
        let item = serde_json::from_value::<ResponseItem>(serde_json::json!({
            "id": "ig_123",
            "type": "image_generation_call",
            "status": "completed",
            "revised_prompt": "A small blue square",
            "result": "Zm9v",
        }))
        .expect("image generation item should deserialize");

        assert_eq!(
            item,
            ResponseItem::ImageGenerationCall {
                id: "ig_123".to_string(),
                status: "completed".to_string(),
                revised_prompt: Some("A small blue square".to_string()),
                result: "Zm9v".to_string(),
            }
        );
    }

    #[test]
    fn response_item_parses_image_generation_call_without_revised_prompt() {
        let item = serde_json::from_value::<ResponseItem>(serde_json::json!({
            "id": "ig_123",
            "type": "image_generation_call",
            "status": "completed",
            "result": "Zm9v",
        }))
        .expect("image generation item should deserialize");

        assert_eq!(
            item,
            ResponseItem::ImageGenerationCall {
                id: "ig_123".to_string(),
                status: "completed".to_string(),
                revised_prompt: None,
                result: "Zm9v".to_string(),
            }
        );
    }

    #[test]
    fn permission_profile_is_empty_when_all_fields_are_none() {
        assert_eq!(PermissionProfile::default().is_empty(), true);
    }

    #[test]
    fn permission_profile_is_not_empty_when_field_is_present_but_nested_empty() {
        let permission_profile = PermissionProfile {
            network: Some(NetworkPermissions { enabled: None }),
            file_system: None,
            macos: None,
        };
        assert_eq!(permission_profile.is_empty(), false);
    }

    #[test]
    fn macos_preferences_permission_deserializes_read_write() {
        let permission = serde_json::from_str::<MacOsPreferencesPermission>("\"read_write\"")
            .expect("deserialize macos preferences permission");
        assert_eq!(permission, MacOsPreferencesPermission::ReadWrite);
    }

    #[test]
    fn macos_preferences_permission_order_matches_permissiveness() {
        assert!(MacOsPreferencesPermission::None < MacOsPreferencesPermission::ReadOnly);
        assert!(MacOsPreferencesPermission::ReadOnly < MacOsPreferencesPermission::ReadWrite);
    }

    #[test]
    fn macos_contacts_permission_order_matches_permissiveness() {
        assert!(MacOsContactsPermission::None < MacOsContactsPermission::ReadOnly);
        assert!(MacOsContactsPermission::ReadOnly < MacOsContactsPermission::ReadWrite);
    }

    #[test]
    fn permission_profile_deserializes_macos_seatbelt_profile_extensions() {
        let permission_profile = serde_json::from_value::<PermissionProfile>(serde_json::json!({
            "network": null,
            "file_system": null,
            "macos": {
                "macos_preferences": "read_write",
                "macos_automation": ["com.apple.Notes"],
                "macos_launch_services": true,
                "macos_accessibility": true,
                "macos_calendar": true
            }
        }))
        .expect("deserialize permission profile");

        assert_eq!(
            permission_profile,
            PermissionProfile {
                network: None,
                file_system: None,
                macos: Some(MacOsSeatbeltProfileExtensions {
                    macos_preferences: MacOsPreferencesPermission::ReadWrite,
                    macos_automation: MacOsAutomationPermission::BundleIds(vec![
                        "com.apple.Notes".to_string(),
                    ]),
                    macos_launch_services: true,
                    macos_accessibility: true,
                    macos_calendar: true,
                    macos_reminders: false,
                    macos_contacts: MacOsContactsPermission::None,
                }),
            }
        );
    }

    #[test]
    fn permission_profile_deserializes_macos_reminders_permission() {
        let permission_profile = serde_json::from_value::<PermissionProfile>(serde_json::json!({
            "macos": {
                "macos_reminders": true
            }
        }))
        .expect("deserialize reminders permission profile");

        assert_eq!(
            permission_profile,
            PermissionProfile {
                network: None,
                file_system: None,
                macos: Some(MacOsSeatbeltProfileExtensions {
                    macos_preferences: MacOsPreferencesPermission::ReadOnly,
                    macos_automation: MacOsAutomationPermission::None,
                    macos_launch_services: false,
                    macos_accessibility: false,
                    macos_calendar: false,
                    macos_reminders: true,
                    macos_contacts: MacOsContactsPermission::None,
                }),
            }
        );
    }

    #[test]
    fn macos_seatbelt_profile_extensions_deserializes_missing_fields_to_defaults() {
        let permissions =
            serde_json::from_value::<MacOsSeatbeltProfileExtensions>(serde_json::json!({
                "macos_automation": ["com.apple.Notes"]
            }))
            .expect("deserialize macos permissions");

        assert_eq!(
            permissions,
            MacOsSeatbeltProfileExtensions {
                macos_preferences: MacOsPreferencesPermission::ReadOnly,
                macos_automation: MacOsAutomationPermission::BundleIds(vec![
                    "com.apple.Notes".to_string(),
                ]),
                macos_launch_services: false,
                macos_accessibility: false,
                macos_calendar: false,
                macos_reminders: false,
                macos_contacts: MacOsContactsPermission::None,
            }
        );
    }

    #[test]
    fn macos_seatbelt_profile_extensions_deserializes_tool_schema_aliases() {
        let permissions =
            serde_json::from_value::<MacOsSeatbeltProfileExtensions>(serde_json::json!({
                "preferences": "read_write",
                "automations": ["com.apple.Notes"],
                "launch_services": true,
                "accessibility": true,
                "calendar": true,
                "reminders": true,
                "contacts": "read_only"
            }))
            .expect("deserialize macos permissions");

        assert_eq!(
            permissions,
            MacOsSeatbeltProfileExtensions {
                macos_preferences: MacOsPreferencesPermission::ReadWrite,
                macos_automation: MacOsAutomationPermission::BundleIds(vec![
                    "com.apple.Notes".to_string(),
                ]),
                macos_launch_services: true,
                macos_accessibility: true,
                macos_calendar: true,
                macos_reminders: true,
                macos_contacts: MacOsContactsPermission::ReadOnly,
            }
        );
    }

    #[test]
    fn macos_automation_permission_deserializes_all_and_none() {
        let all = serde_json::from_str::<MacOsAutomationPermission>("\"all\"")
            .expect("deserialize all automation permission");
        let none = serde_json::from_str::<MacOsAutomationPermission>("\"none\"")
            .expect("deserialize none automation permission");

        assert_eq!(all, MacOsAutomationPermission::All);
        assert_eq!(none, MacOsAutomationPermission::None);
    }

    #[test]
    fn macos_automation_permission_deserializes_bundle_ids_object() {
        let permission = serde_json::from_value::<MacOsAutomationPermission>(serde_json::json!({
            "bundle_ids": ["com.apple.Notes"]
        }))
        .expect("deserialize bundle_ids object automation permission");

        assert_eq!(
            permission,
            MacOsAutomationPermission::BundleIds(vec!["com.apple.Notes".to_string(),])
        );
    }

    #[test]
    fn convert_mcp_content_to_items_builds_data_urls_when_missing_prefix() {
        let contents = vec![serde_json::json!({
            "type": "image",
            "data": "Zm9v",
            "mimeType": "image/png",
        })];

        let items = convert_mcp_content_to_items(&contents).expect("expected image items");
        assert_eq!(
            items,
            vec![FunctionCallOutputContentItem::InputImage {
                image_url: "data:image/png;base64,Zm9v".to_string(),
                detail: None,
            }]
        );
    }

    #[test]
    fn convert_mcp_content_to_items_returns_none_without_images() {
        let contents = vec![serde_json::json!({
            "type": "text",
            "text": "hello",
        })];

        assert_eq!(convert_mcp_content_to_items(&contents), None);
    }

    #[test]
    fn function_call_output_content_items_to_text_joins_text_segments() {
        let content_items = vec![
            FunctionCallOutputContentItem::InputText {
                text: "line 1".to_string(),
            },
            FunctionCallOutputContentItem::InputImage {
                image_url: "data:image/png;base64,AAA".to_string(),
                detail: None,
            },
            FunctionCallOutputContentItem::InputText {
                text: "line 2".to_string(),
            },
        ];

        let text = function_call_output_content_items_to_text(&content_items);
        assert_eq!(text, Some("line 1\nline 2".to_string()));
    }

    #[test]
    fn function_call_output_content_items_to_text_ignores_blank_text_and_images() {
        let content_items = vec![
            FunctionCallOutputContentItem::InputText {
                text: "   ".to_string(),
            },
            FunctionCallOutputContentItem::InputImage {
                image_url: "data:image/png;base64,AAA".to_string(),
                detail: None,
            },
        ];

        let text = function_call_output_content_items_to_text(&content_items);
        assert_eq!(text, None);
    }

    #[test]
    fn function_call_output_body_to_text_returns_plain_text_content() {
        let body = FunctionCallOutputBody::Text("ok".to_string());
        let text = body.to_text();
        assert_eq!(text, Some("ok".to_string()));
    }

    #[test]
    fn function_call_output_body_to_text_uses_content_item_fallback() {
        let body = FunctionCallOutputBody::ContentItems(vec![
            FunctionCallOutputContentItem::InputText {
                text: "line 1".to_string(),
            },
            FunctionCallOutputContentItem::InputImage {
                image_url: "data:image/png;base64,AAA".to_string(),
                detail: None,
            },
        ]);

        let text = body.to_text();
        assert_eq!(text, Some("line 1".to_string()));
    }

    #[test]
    fn function_call_deserializes_optional_namespace() {
        let item: ResponseItem = serde_json::from_value(serde_json::json!({
            "type": "function_call",
            "name": "mcp__codex_apps__gmail_get_recent_emails",
            "namespace": "mcp__codex_apps__gmail",
            "arguments": "{\"top_k\":5}",
            "call_id": "call-1",
        }))
        .expect("function_call should deserialize");

        assert_eq!(
            item,
            ResponseItem::FunctionCall {
                id: None,
                name: "mcp__codex_apps__gmail_get_recent_emails".to_string(),
                namespace: Some("mcp__codex_apps__gmail".to_string()),
                arguments: "{\"top_k\":5}".to_string(),
                call_id: "call-1".to_string(),
            }
        );
    }

    #[test]
    fn converts_sandbox_mode_into_developer_instructions() {
        let workspace_write: DeveloperInstructions = SandboxMode::WorkspaceWrite.into();
        assert_eq!(
            workspace_write,
            DeveloperInstructions::new(
                "Filesystem sandboxing defines which files can be read or written. `sandbox_mode` is `workspace-write`: The sandbox permits reading files, and editing files in `cwd` and `writable_roots`. Editing files in other directories requires approval. Network access is restricted."
            )
        );

        let read_only: DeveloperInstructions = SandboxMode::ReadOnly.into();
        assert_eq!(
            read_only,
            DeveloperInstructions::new(
                "Filesystem sandboxing defines which files can be read or written. `sandbox_mode` is `read-only`: The sandbox only permits reading files. Network access is restricted."
            )
        );
    }

    #[test]
    fn builds_permissions_with_network_access_override() {
        let instructions = DeveloperInstructions::from_permissions_with_network(
            SandboxMode::WorkspaceWrite,
            NetworkAccess::Enabled,
            ApprovalPolicy::Interactive,
            &Policy::empty(),
            None,
            false,
            false,
        );

        let text = instructions.into_text();
        assert!(
            text.contains("Network access is enabled."),
            "expected network access to be enabled in message"
        );
        assert!(
            text.contains("How to request escalation"),
            "expected approval guidance to be included"
        );
    }

    #[test]
    fn builds_permissions_from_policy() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            read_only_access: Default::default(),
            network_access: true,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        };

        let instructions = DeveloperInstructions::from_policy(
            &policy,
            ApprovalPolicy::Supervised,
            &Policy::empty(),
            &PathBuf::from("/tmp"),
            false,
            false,
        );
        let text = instructions.into_text();
        assert!(text.contains("Network access is enabled."));
        assert!(text.contains("`approval_policy` is `unless-trusted`"));
    }

    fn permissions_prompt_text(
        approval_policy: ApprovalPolicy,
        exec_policy: &Policy,
        include_shell_permission_request_instructions: bool,
        include_request_permissions_tool_section: bool,
    ) -> String {
        DeveloperInstructions::from_permissions_with_network(
            SandboxMode::WorkspaceWrite,
            NetworkAccess::Enabled,
            approval_policy,
            exec_policy,
            None,
            include_shell_permission_request_instructions,
            include_request_permissions_tool_section,
        )
        .into_text()
    }

    fn granular_policy_text(
        config: GranularApprovalConfig,
        include_shell_permission_request_instructions: bool,
        include_request_permissions_tool_section: bool,
    ) -> String {
        DeveloperInstructions::from(
            ApprovalPolicy::Granular(config),
            &Policy::empty(),
            include_shell_permission_request_instructions,
            include_request_permissions_tool_section,
        )
        .into_text()
    }

    #[track_caller]
    fn assert_contains_all(text: &str, expected: &[&str]) {
        for needle in expected {
            assert!(text.contains(needle), "expected {needle:?} in:\n{text}");
        }
    }

    #[test]
    fn includes_request_rule_instructions_for_on_request() {
        let mut exec_policy = Policy::empty();
        exec_policy
            .add_prefix_rule(
                &["git".to_string(), "pull".to_string()],
                chaos_selinux::Decision::Allow,
            )
            .expect("add rule");

        let text = permissions_prompt_text(ApprovalPolicy::Interactive, &exec_policy, false, false);
        assert_contains_all(
            &text,
            &[
                "prefix_rule",
                "Approved command prefixes",
                r#"["git", "pull"]"#,
            ],
        );
    }

    #[test]
    fn request_permissions_sections_follow_policy_and_enabled_flags() {
        struct Case {
            name: &'static str,
            approval_policy: ApprovalPolicy,
            include_shell_permission_request_instructions: bool,
            include_request_permissions_tool_section: bool,
            expected: &'static [&'static str],
            unexpected: &'static [&'static str],
        }

        let cases = [
            Case {
                name: "unless-trusted with tool guidance",
                approval_policy: ApprovalPolicy::Supervised,
                include_shell_permission_request_instructions: false,
                include_request_permissions_tool_section: true,
                expected: &[
                    "`approval_policy` is `unless-trusted`",
                    "# request_permissions Tool",
                ],
                unexpected: &["with_additional_permissions"],
            },
            Case {
                name: "interactive with tool guidance",
                approval_policy: ApprovalPolicy::Interactive,
                include_shell_permission_request_instructions: false,
                include_request_permissions_tool_section: true,
                expected: &[
                    "# request_permissions Tool",
                    "The built-in `request_permissions` tool is available in this session.",
                ],
                unexpected: &["with_additional_permissions"],
            },
            Case {
                name: "interactive with inline shell guidance",
                approval_policy: ApprovalPolicy::Interactive,
                include_shell_permission_request_instructions: true,
                include_request_permissions_tool_section: false,
                expected: &["with_additional_permissions", "additional_permissions"],
                unexpected: &["# request_permissions Tool"],
            },
            Case {
                name: "interactive with both inline and tool guidance",
                approval_policy: ApprovalPolicy::Interactive,
                include_shell_permission_request_instructions: true,
                include_request_permissions_tool_section: true,
                expected: &["with_additional_permissions", "# request_permissions Tool"],
                unexpected: &[],
            },
        ];

        for case in cases {
            let text = permissions_prompt_text(
                case.approval_policy,
                &Policy::empty(),
                case.include_shell_permission_request_instructions,
                case.include_request_permissions_tool_section,
            );

            for needle in case.expected {
                assert!(
                    text.contains(needle),
                    "case {} expected {needle:?} in:\n{text}",
                    case.name
                );
            }
            for needle in case.unexpected {
                assert!(
                    !text.contains(needle),
                    "case {} did not expect {needle:?} in:\n{text}",
                    case.name
                );
            }
        }
    }

    fn granular_categories_section(title: &str, categories: &[&str]) -> String {
        format!("{title}\n{}", categories.join("\n"))
    }

    fn granular_prompt_expected(
        prompted_categories: &[&str],
        rejected_categories: &[&str],
        include_shell_permission_request_instructions: bool,
        include_request_permissions_tool_section: bool,
    ) -> String {
        let mut sections = vec![granular_prompt_intro_text().to_string()];
        if !prompted_categories.is_empty() {
            sections.push(granular_categories_section(
                "These approval categories may still prompt the user when needed:",
                prompted_categories,
            ));
        }
        if !rejected_categories.is_empty() {
            sections.push(granular_categories_section(
                "These approval categories are automatically rejected instead of prompting the user:",
                rejected_categories,
            ));
        }
        if include_shell_permission_request_instructions {
            sections.push(APPROVAL_POLICY_ON_REQUEST_RULE_REQUEST_PERMISSION.to_string());
        }
        if include_request_permissions_tool_section {
            sections.push(request_permissions_tool_prompt_section().to_string());
        }
        sections.join("\n\n")
    }

    #[test]
    fn granular_policy_exact_prompt_variants() {
        struct Case {
            name: &'static str,
            config: GranularApprovalConfig,
            include_shell_permission_request_instructions: bool,
            include_request_permissions_tool_section: bool,
            expected: String,
        }

        let cases = [
            Case {
                name: "separates prompted and rejected categories",
                config: GranularApprovalConfig {
                    sandbox_approval: false,
                    rules: true,
                    request_permissions: true,
                    mcp_elicitations: false,
                },
                include_shell_permission_request_instructions: true,
                include_request_permissions_tool_section: false,
                expected: [
                    granular_prompt_intro_text().to_string(),
                    granular_categories_section(
                        "These approval categories may still prompt the user when needed:",
                        &["- `rules`"],
                    ),
                    granular_categories_section(
                        "These approval categories are automatically rejected instead of prompting the user:",
                        &["- `sandbox_approval`", "- `mcp_elicitations`"],
                    ),
                ]
                .join("\n\n"),
            },
            Case {
                name: "includes inline shell permission guidance when sandbox approval can prompt",
                config: GranularApprovalConfig {
                    sandbox_approval: true,
                    rules: true,
                    request_permissions: true,
                    mcp_elicitations: true,
                },
                include_shell_permission_request_instructions: true,
                include_request_permissions_tool_section: false,
                expected: granular_prompt_expected(
                    &[
                        "- `sandbox_approval`",
                        "- `rules`",
                        "- `mcp_elicitations`",
                    ],
                    &[],
                    true,
                    false,
                ),
            },
            Case {
                name: "omits inline shell permission guidance when disabled",
                config: GranularApprovalConfig {
                    sandbox_approval: true,
                    rules: true,
                    request_permissions: true,
                    mcp_elicitations: true,
                },
                include_shell_permission_request_instructions: false,
                include_request_permissions_tool_section: false,
                expected: granular_prompt_expected(
                    &[
                        "- `sandbox_approval`",
                        "- `rules`",
                        "- `mcp_elicitations`",
                    ],
                    &[],
                    false,
                    false,
                ),
            },
        ];

        for case in cases {
            let text = granular_policy_text(
                case.config,
                case.include_shell_permission_request_instructions,
                case.include_request_permissions_tool_section,
            );

            assert_eq!(text, case.expected, "case: {}", case.name);
        }
    }

    #[test]
    fn granular_policy_request_permissions_tool_visibility_matches_promptability() {
        struct Case {
            name: &'static str,
            config: GranularApprovalConfig,
            include_request_permissions_tool_section: bool,
            expected: &'static [&'static str],
            unexpected: &'static [&'static str],
        }

        let cases = [
            Case {
                name: "tool section appears when request_permissions can still prompt",
                config: GranularApprovalConfig {
                    sandbox_approval: true,
                    rules: true,
                    request_permissions: true,
                    mcp_elicitations: true,
                },
                include_request_permissions_tool_section: true,
                expected: &["# request_permissions Tool"],
                unexpected: &[],
            },
            Case {
                name: "tool section is omitted when request_permissions prompting is rejected",
                config: GranularApprovalConfig {
                    sandbox_approval: true,
                    rules: true,
                    request_permissions: false,
                    mcp_elicitations: true,
                },
                include_request_permissions_tool_section: true,
                expected: &[],
                unexpected: &["# request_permissions Tool"],
            },
            Case {
                name: "request_permissions category stays hidden when tool is unavailable",
                config: GranularApprovalConfig {
                    sandbox_approval: false,
                    rules: false,
                    request_permissions: true,
                    mcp_elicitations: false,
                },
                include_request_permissions_tool_section: false,
                expected: &[],
                unexpected: &["- `request_permissions`", "# request_permissions Tool"],
            },
        ];

        for case in cases {
            let text = granular_policy_text(
                case.config,
                true,
                case.include_request_permissions_tool_section,
            );

            for needle in case.expected {
                assert!(
                    text.contains(needle),
                    "case {} expected {needle:?} in:\n{text}",
                    case.name
                );
            }
            for needle in case.unexpected {
                assert!(
                    !text.contains(needle),
                    "case {} did not expect {needle:?} in:\n{text}",
                    case.name
                );
            }
        }
    }

    #[test]
    fn render_command_prefix_list_sorts_by_len_then_total_len_then_alphabetical() {
        let prefixes = vec![
            vec!["b".to_string(), "zz".to_string()],
            vec!["aa".to_string()],
            vec!["b".to_string()],
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            vec!["a".to_string()],
            vec!["b".to_string(), "a".to_string()],
        ];

        let output = format_allow_prefixes(prefixes).expect("rendered list");
        assert_eq!(
            output,
            r#"- ["a"]
- ["b"]
- ["aa"]
- ["b", "a"]
- ["b", "zz"]
- ["a", "b", "c"]"#
                .to_string(),
        );
    }

    #[test]
    fn render_command_prefix_list_limits_output_to_max_prefixes() {
        let prefixes = (0..(MAX_RENDERED_PREFIXES + 5))
            .map(|i| vec![format!("{i:03}")])
            .collect::<Vec<_>>();

        let output = format_allow_prefixes(prefixes).expect("rendered list");
        assert_eq!(output.ends_with(TRUNCATED_MARKER), true);
        eprintln!("output: {output}");
        assert_eq!(output.lines().count(), MAX_RENDERED_PREFIXES + 1);
    }

    #[test]
    fn format_allow_prefixes_limits_output() {
        let mut exec_policy = Policy::empty();
        for i in 0..200 {
            exec_policy
                .add_prefix_rule(
                    &[format!("tool-{i:03}"), "x".repeat(500)],
                    chaos_selinux::Decision::Allow,
                )
                .expect("add rule");
        }

        let output =
            format_allow_prefixes(exec_policy.get_allowed_prefixes()).expect("formatted prefixes");
        assert!(
            output.len() <= MAX_ALLOW_PREFIX_TEXT_BYTES + TRUNCATED_MARKER.len(),
            "output length exceeds expected limit: {output}",
        );
    }

    #[test]
    fn serializes_success_as_plain_string() -> Result<()> {
        let item = ResponseInputItem::FunctionCallOutput {
            call_id: "call1".into(),
            output: FunctionCallOutputPayload::from_text("ok".into()),
            tool_name: None,
        };

        let json = serde_json::to_string(&item)?;
        let v: serde_json::Value = serde_json::from_str(&json)?;

        // Success case -> output should be a plain string
        assert_eq!(v.get("output").unwrap().as_str().unwrap(), "ok");
        Ok(())
    }

    #[test]
    fn serializes_failure_as_string() -> Result<()> {
        let item = ResponseInputItem::FunctionCallOutput {
            call_id: "call1".into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("bad".into()),
                success: Some(false),
            },
            tool_name: None,
        };

        let json = serde_json::to_string(&item)?;
        let v: serde_json::Value = serde_json::from_str(&json)?;

        assert_eq!(v.get("output").unwrap().as_str().unwrap(), "bad");
        Ok(())
    }

    #[test]
    fn serializes_image_outputs_as_array() -> Result<()> {
        let call_tool_result = CallToolResult {
            content: vec![
                serde_json::json!({"type":"text","text":"caption"}),
                serde_json::json!({"type":"image","data":"BASE64","mimeType":"image/png"}),
            ],
            structured_content: None,
            is_error: Some(false),
            meta: None,
        };

        let payload = call_tool_result.into_function_call_output_payload();
        assert_eq!(payload.success, Some(true));
        let Some(items) = payload.content_items() else {
            panic!("expected content items");
        };
        let items = items.to_vec();
        assert_eq!(
            items,
            vec![
                FunctionCallOutputContentItem::InputText {
                    text: "caption".into(),
                },
                FunctionCallOutputContentItem::InputImage {
                    image_url: "data:image/png;base64,BASE64".into(),
                    detail: None,
                },
            ]
        );

        let item = ResponseInputItem::FunctionCallOutput {
            call_id: "call1".into(),
            output: payload,
            tool_name: None,
        };

        let json = serde_json::to_string(&item)?;
        let v: serde_json::Value = serde_json::from_str(&json)?;

        let output = v.get("output").expect("output field");
        assert!(output.is_array(), "expected array output");

        Ok(())
    }

    #[test]
    fn serializes_custom_tool_image_outputs_as_array() -> Result<()> {
        let item = ResponseInputItem::CustomToolCallOutput {
            call_id: "call1".into(),
            output: FunctionCallOutputPayload::from_content_items(vec![
                FunctionCallOutputContentItem::InputImage {
                    image_url: "data:image/png;base64,BASE64".into(),
                    detail: None,
                },
            ]),
            tool_name: None,
        };

        let json = serde_json::to_string(&item)?;
        let v: serde_json::Value = serde_json::from_str(&json)?;

        let output = v.get("output").expect("output field");
        assert!(output.is_array(), "expected array output");

        Ok(())
    }

    #[test]
    fn preserves_existing_image_data_urls() -> Result<()> {
        let call_tool_result = CallToolResult {
            content: vec![serde_json::json!({
                "type": "image",
                "data": "data:image/png;base64,BASE64",
                "mimeType": "image/png"
            })],
            structured_content: None,
            is_error: Some(false),
            meta: None,
        };

        let payload = call_tool_result.into_function_call_output_payload();
        let Some(items) = payload.content_items() else {
            panic!("expected content items");
        };
        let items = items.to_vec();
        assert_eq!(
            items,
            vec![FunctionCallOutputContentItem::InputImage {
                image_url: "data:image/png;base64,BASE64".into(),
                detail: None,
            }]
        );

        Ok(())
    }

    #[test]
    fn deserializes_array_payload_into_items() -> Result<()> {
        let json = r#"[
            {"type": "input_text", "text": "note"},
            {"type": "input_image", "image_url": "data:image/png;base64,XYZ"}
        ]"#;

        let payload: FunctionCallOutputPayload = serde_json::from_str(json)?;

        assert_eq!(payload.success, None);
        let expected_items = vec![
            FunctionCallOutputContentItem::InputText {
                text: "note".into(),
            },
            FunctionCallOutputContentItem::InputImage {
                image_url: "data:image/png;base64,XYZ".into(),
                detail: None,
            },
        ];
        assert_eq!(
            payload.body,
            FunctionCallOutputBody::ContentItems(expected_items.clone())
        );
        assert_eq!(
            serde_json::to_string(&payload)?,
            serde_json::to_string(&expected_items)?
        );

        Ok(())
    }

    #[test]
    fn deserializes_compaction_alias() -> Result<()> {
        let json = r#"{"type":"compaction_summary","encrypted_content":"abc"}"#;

        let item: ResponseItem = serde_json::from_str(json)?;

        assert_eq!(
            item,
            ResponseItem::Compaction {
                encrypted_content: "abc".into(),
            }
        );
        Ok(())
    }

    #[test]
    fn roundtrips_web_search_call_actions() -> Result<()> {
        let cases = vec![
            (
                r#"{
                    "type": "web_search_call",
                    "status": "completed",
                    "action": {
                        "type": "search",
                        "query": "weather seattle",
                        "queries": ["weather seattle", "seattle weather now"]
                    }
                }"#,
                None,
                Some(WebSearchAction::Search {
                    query: Some("weather seattle".into()),
                    queries: Some(vec!["weather seattle".into(), "seattle weather now".into()]),
                }),
                Some("completed".into()),
                true,
            ),
            (
                r#"{
                    "type": "web_search_call",
                    "status": "open",
                    "action": {
                        "type": "open_page",
                        "url": "https://example.com"
                    }
                }"#,
                None,
                Some(WebSearchAction::OpenPage {
                    url: Some("https://example.com".into()),
                }),
                Some("open".into()),
                true,
            ),
            (
                r#"{
                    "type": "web_search_call",
                    "status": "in_progress",
                    "action": {
                        "type": "find_in_page",
                        "url": "https://example.com/docs",
                        "pattern": "installation"
                    }
                }"#,
                None,
                Some(WebSearchAction::FindInPage {
                    url: Some("https://example.com/docs".into()),
                    pattern: Some("installation".into()),
                }),
                Some("in_progress".into()),
                true,
            ),
            (
                r#"{
                    "type": "web_search_call",
                    "status": "in_progress",
                    "id": "ws_partial"
                }"#,
                Some("ws_partial".into()),
                None,
                Some("in_progress".into()),
                false,
            ),
        ];

        for (json_literal, expected_id, expected_action, expected_status, expect_roundtrip) in cases
        {
            let parsed: ResponseItem = serde_json::from_str(json_literal)?;
            let expected = ResponseItem::WebSearchCall {
                id: expected_id.clone(),
                status: expected_status.clone(),
                action: expected_action.clone(),
            };
            assert_eq!(parsed, expected);

            let serialized = serde_json::to_value(&parsed)?;
            let mut expected_serialized: serde_json::Value = serde_json::from_str(json_literal)?;
            if !expect_roundtrip && let Some(obj) = expected_serialized.as_object_mut() {
                obj.remove("id");
            }
            assert_eq!(serialized, expected_serialized);
        }

        Ok(())
    }

    #[test]
    fn deserialize_shell_tool_call_params() -> Result<()> {
        let json = r#"{
            "command": ["ls", "-l"],
            "workdir": "/tmp",
            "timeout": 1000
        }"#;

        let params: ShellToolCallParams = serde_json::from_str(json)?;
        assert_eq!(
            ShellToolCallParams {
                command: vec!["ls".to_string(), "-l".to_string()],
                workdir: Some("/tmp".to_string()),
                timeout_ms: Some(1000),
                sandbox_permissions: None,
                prefix_rule: None,
                additional_permissions: None,
                justification: None,
            },
            params
        );
        Ok(())
    }

    #[test]
    fn wraps_image_user_input_with_tags() -> Result<()> {
        let image_url = "data:image/png;base64,abc".to_string();

        let item = ResponseInputItem::from(vec![UserInput::Image {
            image_url: image_url.clone(),
        }]);

        match item {
            ResponseInputItem::Message { content, .. } => {
                let expected = vec![
                    ContentItem::InputText {
                        text: image_open_tag_text(),
                    },
                    ContentItem::InputImage { image_url },
                    ContentItem::InputText {
                        text: image_close_tag_text(),
                    },
                ];
                assert_eq!(content, expected);
            }
            other => panic!("expected message response but got {other:?}"),
        }

        Ok(())
    }

    #[test]
    fn tool_search_call_roundtrips() -> Result<()> {
        let parsed: ResponseItem = serde_json::from_str(
            r#"{
                "type": "tool_search_call",
                "call_id": "search-1",
                "execution": "client",
                "arguments": {
                    "query": "calendar create",
                    "limit": 1
                }
            }"#,
        )?;

        assert_eq!(
            parsed,
            ResponseItem::ToolSearchCall {
                id: None,
                call_id: Some("search-1".to_string()),
                status: None,
                execution: "client".to_string(),
                arguments: serde_json::json!({
                    "query": "calendar create",
                    "limit": 1,
                }),
            }
        );

        assert_eq!(
            serde_json::to_value(&parsed)?,
            serde_json::json!({
                "type": "tool_search_call",
                "call_id": "search-1",
                "execution": "client",
                "arguments": {
                    "query": "calendar create",
                    "limit": 1,
                }
            })
        );

        Ok(())
    }

    #[test]
    fn tool_search_output_roundtrips() -> Result<()> {
        let input = ResponseInputItem::ToolSearchOutput {
            call_id: "search-1".to_string(),
            status: "completed".to_string(),
            execution: "client".to_string(),
            tools: vec![serde_json::json!({
                "type": "function",
                "name": "mcp__codex_apps__calendar_create_event",
                "description": "Create a calendar event.",
                "defer_loading": true,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "title": {"type": "string"}
                    },
                    "required": ["title"],
                    "additionalProperties": false,
                }
            })],
        };
        assert_eq!(
            ResponseItem::from(input.clone()),
            ResponseItem::ToolSearchOutput {
                call_id: Some("search-1".to_string()),
                status: "completed".to_string(),
                execution: "client".to_string(),
                tools: vec![serde_json::json!({
                    "type": "function",
                    "name": "mcp__codex_apps__calendar_create_event",
                    "description": "Create a calendar event.",
                    "defer_loading": true,
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "title": {"type": "string"}
                        },
                        "required": ["title"],
                        "additionalProperties": false,
                    }
                })],
            }
        );

        assert_eq!(
            serde_json::to_value(input)?,
            serde_json::json!({
                "type": "tool_search_output",
                "call_id": "search-1",
                "status": "completed",
                "execution": "client",
                "tools": [{
                    "type": "function",
                    "name": "mcp__codex_apps__calendar_create_event",
                    "description": "Create a calendar event.",
                    "defer_loading": true,
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "title": {"type": "string"}
                        },
                        "required": ["title"],
                        "additionalProperties": false,
                    }
                }]
            })
        );

        Ok(())
    }

    #[test]
    fn tool_search_server_items_allow_null_call_id() -> Result<()> {
        let parsed_call: ResponseItem = serde_json::from_str(
            r#"{
                "type": "tool_search_call",
                "execution": "server",
                "call_id": null,
                "status": "completed",
                "arguments": {
                    "paths": ["crm"]
                }
            }"#,
        )?;
        assert_eq!(
            parsed_call,
            ResponseItem::ToolSearchCall {
                id: None,
                call_id: None,
                status: Some("completed".to_string()),
                execution: "server".to_string(),
                arguments: serde_json::json!({
                    "paths": ["crm"],
                }),
            }
        );

        let parsed_output: ResponseItem = serde_json::from_str(
            r#"{
                "type": "tool_search_output",
                "execution": "server",
                "call_id": null,
                "status": "completed",
                "tools": []
            }"#,
        )?;
        assert_eq!(
            parsed_output,
            ResponseItem::ToolSearchOutput {
                call_id: None,
                status: "completed".to_string(),
                execution: "server".to_string(),
                tools: vec![],
            }
        );

        Ok(())
    }

    #[test]
    fn mixed_remote_and_local_images_share_label_sequence() -> Result<()> {
        let image_url = "data:image/png;base64,abc".to_string();
        let dir = tempdir()?;
        let local_path = dir.path().join("local.png");
        // A tiny valid PNG (1x1) so this test doesn't depend on cross-crate file paths, which
        // break under Bazel sandboxing.
        const TINY_PNG_BYTES: &[u8] = &[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 11, 73, 68, 65, 84, 120, 156, 99, 96, 0, 2,
            0, 0, 5, 0, 1, 122, 94, 171, 63, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
        ];
        std::fs::write(&local_path, TINY_PNG_BYTES)?;

        let item = ResponseInputItem::from(vec![
            UserInput::Image {
                image_url: image_url.clone(),
            },
            UserInput::LocalImage { path: local_path },
        ]);

        match item {
            ResponseInputItem::Message { content, .. } => {
                assert_eq!(
                    content.first(),
                    Some(&ContentItem::InputText {
                        text: image_open_tag_text(),
                    })
                );
                assert_eq!(content.get(1), Some(&ContentItem::InputImage { image_url }));
                assert_eq!(
                    content.get(2),
                    Some(&ContentItem::InputText {
                        text: image_close_tag_text(),
                    })
                );
                assert_eq!(
                    content.get(3),
                    Some(&ContentItem::InputText {
                        text: local_image_open_tag_text(2),
                    })
                );
                assert!(matches!(
                    content.get(4),
                    Some(ContentItem::InputImage { .. })
                ));
                assert_eq!(
                    content.get(5),
                    Some(&ContentItem::InputText {
                        text: image_close_tag_text(),
                    })
                );
            }
            other => panic!("expected message response but got {other:?}"),
        }

        Ok(())
    }

    #[test]
    fn local_image_read_error_adds_placeholder() -> Result<()> {
        let dir = tempdir()?;
        let missing_path = dir.path().join("missing-image.png");

        let item = ResponseInputItem::from(vec![UserInput::LocalImage {
            path: missing_path.clone(),
        }]);

        match item {
            ResponseInputItem::Message { content, .. } => {
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ContentItem::InputText { text } => {
                        let display_path = missing_path.display().to_string();
                        assert!(
                            text.contains(&display_path),
                            "placeholder should mention missing path: {text}"
                        );
                        assert!(
                            text.contains("could not read"),
                            "placeholder should mention read issue: {text}"
                        );
                    }
                    other => panic!("expected placeholder text but found {other:?}"),
                }
            }
            other => panic!("expected message response but got {other:?}"),
        }

        Ok(())
    }

    #[test]
    fn local_image_non_image_adds_placeholder() -> Result<()> {
        let dir = tempdir()?;
        let json_path = dir.path().join("example.json");
        std::fs::write(&json_path, br#"{"hello":"world"}"#)?;

        let item = ResponseInputItem::from(vec![UserInput::LocalImage {
            path: json_path.clone(),
        }]);

        match item {
            ResponseInputItem::Message { content, .. } => {
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ContentItem::InputText { text } => {
                        assert!(
                            text.contains("unsupported MIME type `application/json`"),
                            "placeholder should mention unsupported MIME: {text}"
                        );
                        assert!(
                            text.contains(&json_path.display().to_string()),
                            "placeholder should mention path: {text}"
                        );
                    }
                    other => panic!("expected placeholder text but found {other:?}"),
                }
            }
            other => panic!("expected message response but got {other:?}"),
        }

        Ok(())
    }

    #[test]
    fn local_image_unsupported_image_format_adds_placeholder() -> Result<()> {
        let dir = tempdir()?;
        let svg_path = dir.path().join("example.svg");
        std::fs::write(
            &svg_path,
            br#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"></svg>"#,
        )?;

        let item = ResponseInputItem::from(vec![UserInput::LocalImage {
            path: svg_path.clone(),
        }]);

        match item {
            ResponseInputItem::Message { content, .. } => {
                assert_eq!(content.len(), 1);
                let expected = format!(
                    "Chaos cannot attach image at `{}`: unsupported image format `image/svg+xml`.",
                    svg_path.display()
                );
                match &content[0] {
                    ContentItem::InputText { text } => assert_eq!(text, &expected),
                    other => panic!("expected placeholder text but found {other:?}"),
                }
            }
            other => panic!("expected message response but got {other:?}"),
        }

        Ok(())
    }
}
