//! Transcript/history cells for the Chaos TUI.
//!
//! A `HistoryCell` is the unit of display in the conversation UI, representing both committed
//! transcript entries and, transiently, an in-flight active cell that can mutate in place while
//! streaming.
//!
//! The transcript overlay (`Ctrl+T`) appends a cached live tail derived from the active cell, and
//! that cached tail is refreshed based on an active-cell cache key. Cells that change based on
//! elapsed time expose `transcript_animation_tick()`, and code that mutates the active cell in place
//! bumps the active-cell revision tracked by `ChatWidget`, so the cache key changes whenever the
//! rendered transcript output can change.

mod diff;
mod render;
mod state;

pub use diff::PatchHistoryCell;
pub use diff::new_patch_apply_failure;
pub use diff::new_patch_event;
pub use diff::new_view_image_tool_call;

pub use render::runtime_metrics_label;
pub use render::with_border_with_inner_width;

pub use state::AgentMessageCell;
pub use state::ApprovalDecisionActor;
pub use state::CompositeHistoryCell;
pub use state::DeprecationNoticeCell;
pub use state::FinalMessageSeparator;
pub use state::HistoryCell;
pub use state::McpToolCallCell;
pub use state::PlainHistoryCell;
pub use state::PlanUpdateCell;
pub use state::PrefixedWrappedHistoryCell;
pub use state::ProposedPlanCell;
pub use state::ProposedPlanStreamCell;
pub use state::ReasoningSummaryCell;
pub use state::RequestUserInputResultCell;
pub use state::SessionInfoCell;
pub use state::UnifiedExecInteractionCell;
pub use state::UnifiedExecProcessDetails;
pub use state::UserHistoryCell;
pub use state::WebSearchCell;
pub use state::empty_mcp_output;
pub use state::new_active_mcp_tool_call;
pub use state::new_active_web_search_call;
pub use state::new_approval_decision_cell;
pub use state::new_deprecation_notice;
pub use state::new_error_event;
pub use state::new_image_generation_call;
pub use state::new_info_event;
pub use state::new_mcp_tools_output;
pub use state::new_plan_update;
pub use state::new_proposed_plan;
pub use state::new_proposed_plan_stream;
pub use state::new_reasoning_summary_block;
pub use state::new_review_status_line;
pub use state::new_session_info;
pub use state::new_unified_exec_interaction;
pub use state::new_unified_exec_processes_output;
pub use state::new_user_prompt;
pub use state::new_warning_event;
pub use state::new_web_search_call;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec_cell::CommandOutput;
    use crate::exec_cell::ExecCall;
    use crate::exec_cell::ExecCell;
    use chaos_ipc::ProcessId;
    use chaos_ipc::models::WebSearchAction;
    use chaos_ipc::parse_command::ParsedCommand;
    use chaos_ipc::protocol::ApprovalPolicy;
    use chaos_ipc::protocol::McpAuthStatus;
    use chaos_ipc::protocol::SandboxPolicy;
    use chaos_ipc::protocol::SessionConfiguredEvent;
    use chaos_kern::config::Config;
    use chaos_kern::config::ConfigBuilder;
    use chaos_kern::config::types::McpServerConfig;
    use chaos_kern::config::types::McpServerTransportConfig;
    use chaos_syslog::RuntimeMetricTotals;
    use chaos_syslog::RuntimeMetricsSummary;

    use crate::render::renderable::Renderable;
    use chaos_ipc::protocol::McpInvocation;
    use pretty_assertions::assert_eq;
    use ratatui::prelude::*;
    use ratatui::widgets::Paragraph;
    use ratatui::widgets::Wrap;
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;
    use std::time::Instant;

    use chaos_ipc::mcp::CallToolResult;
    use chaos_ipc::mcp::Tool;
    use chaos_ipc::protocol::ExecCommandSource;
    use mcp_guest::ContentBlock;

    const SMALL_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
    async fn test_config() -> Config {
        let chaos_home = std::env::temp_dir();
        ConfigBuilder::default()
            .chaos_home(chaos_home.clone())
            .build()
            .await
            .expect("config")
    }

    fn test_cwd() -> PathBuf {
        std::env::temp_dir()
    }

    fn render_lines(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn render_transcript(cell: &dyn HistoryCell) -> Vec<String> {
        render_lines(&cell.transcript_lines(u16::MAX))
    }

    fn image_block(data: &str) -> serde_json::Value {
        serde_json::to_value(ContentBlock::Image {
            data: data.to_string(),
            mime_type: "image/png".to_string(),
            annotations: None,
            meta: None,
        })
        .expect("image content should serialize")
    }

    fn text_block(text: &str) -> serde_json::Value {
        serde_json::to_value(ContentBlock::text(text)).expect("text content should serialize")
    }

    fn resource_link_block(
        uri: &str,
        name: &str,
        title: Option<&str>,
        description: Option<&str>,
    ) -> serde_json::Value {
        serde_json::to_value(ContentBlock::ResourceLink {
            uri: uri.to_string(),
            name: name.to_string(),
            title: title.map(str::to_string),
            description: description.map(str::to_string),
            mime_type: None,
            size: None,
            icons: None,
            annotations: None,
            meta: None,
        })
        .expect("resource link content should serialize")
    }

    #[expect(dead_code, reason = "test helper available for future tests")]
    fn session_configured_event(model: &str) -> SessionConfiguredEvent {
        SessionConfiguredEvent {
            session_id: ProcessId::new(),
            forked_from_id: None,
            process_name: None,
            model: model.to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: chaos_ipc::config_types::ApprovalsReviewer::User,
            vfs_policy: chaos_ipc::protocol::VfsPolicy::from(&SandboxPolicy::new_read_only_policy()),
            socket_policy: chaos_ipc::protocol::SocketPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/tmp/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }
    }

    #[test]
    fn unified_exec_interaction_cell_renders_input() {
        let cell =
            new_unified_exec_interaction(Some("echo hello".to_string()), "ls\npwd".to_string());
        let lines = render_transcript(&cell);
        assert_eq!(
            lines,
            vec![
                "↳ Interacted with background terminal · echo hello",
                "  └ ls",
                "    pwd",
            ],
        );
    }

    #[test]
    fn unified_exec_interaction_cell_renders_wait() {
        let cell = new_unified_exec_interaction(None, String::new());
        let lines = render_transcript(&cell);
        assert_eq!(lines, vec!["• Waited for background terminal"]);
    }

    #[test]
    fn final_message_separator_hides_short_worked_label_and_includes_runtime_metrics() {
        let summary = RuntimeMetricsSummary {
            tool_calls: RuntimeMetricTotals {
                count: 3,
                duration_ms: 2_450,
            },
            api_calls: RuntimeMetricTotals {
                count: 2,
                duration_ms: 1_200,
            },
            streaming_events: RuntimeMetricTotals {
                count: 6,
                duration_ms: 900,
            },
            responses_api_overhead_ms: 650,
            responses_api_inference_time_ms: 1_940,
            responses_api_engine_iapi_ttft_ms: 410,
            responses_api_engine_service_ttft_ms: 460,
            responses_api_engine_iapi_tbt_ms: 1_180,
            responses_api_engine_service_tbt_ms: 1_240,
            turn_ttft_ms: 0,
            turn_ttfm_ms: 0,
        };
        let cell = FinalMessageSeparator::new(Some(12), Some(summary));
        let rendered = render_lines(&cell.display_lines(600));

        assert_eq!(rendered.len(), 1);
        assert!(!rendered[0].contains("Worked for"));
        assert!(rendered[0].contains("Local tools: 3 calls (2.5s)"));
        assert!(rendered[0].contains("Inference: 2 calls (1.2s)"));
        assert!(rendered[0].contains("Streams: 6 events (900ms)"));
        assert!(rendered[0].contains("Responses API overhead: 650ms"));
        assert!(rendered[0].contains("Responses API inference: 1.9s"));
        assert!(rendered[0].contains("TTFT: 410ms (iapi) 460ms (service)"));
        assert!(rendered[0].contains("TBT: 1.2s (iapi) 1.2s (service)"));
    }

    #[test]
    fn final_message_separator_includes_worked_label_after_one_minute() {
        let cell = FinalMessageSeparator::new(Some(61), None);
        let rendered = render_lines(&cell.display_lines(200));

        assert_eq!(rendered.len(), 1);
        assert!(rendered[0].contains("Worked for"));
    }

    #[test]
    fn ps_output_empty_snapshot() {
        let cell = new_unified_exec_processes_output(Vec::new());
        let rendered = render_lines(&cell.display_lines(60)).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn ps_output_multiline_snapshot() {
        let cell = new_unified_exec_processes_output(vec![
            UnifiedExecProcessDetails {
                command_display: "echo hello\nand then some extra text".to_string(),
                recent_chunks: vec!["hello".to_string(), "done".to_string()],
            },
            UnifiedExecProcessDetails {
                command_display: "rg \"foo\" src".to_string(),
                recent_chunks: vec!["src/main.rs:12:foo".to_string()],
            },
        ]);
        let rendered = render_lines(&cell.display_lines(40)).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn ps_output_long_command_snapshot() {
        let cell = new_unified_exec_processes_output(vec![UnifiedExecProcessDetails {
            command_display: String::from(
                "rg \"foo\" src --glob '**/*.rs' --max-count 1000 --no-ignore --hidden --follow --glob '!target/**'",
            ),
            recent_chunks: vec!["searching...".to_string()],
        }]);
        let rendered = render_lines(&cell.display_lines(36)).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn ps_output_many_sessions_snapshot() {
        let cell = new_unified_exec_processes_output(
            (0..20)
                .map(|idx| UnifiedExecProcessDetails {
                    command_display: format!("command {idx}"),
                    recent_chunks: Vec::new(),
                })
                .collect(),
        );
        let rendered = render_lines(&cell.display_lines(32)).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn ps_output_chunk_leading_whitespace_snapshot() {
        let cell = new_unified_exec_processes_output(vec![UnifiedExecProcessDetails {
            command_display: "just fix".to_string(),
            recent_chunks: vec![
                "  indented first".to_string(),
                "    more indented".to_string(),
            ],
        }]);
        let rendered = render_lines(&cell.display_lines(60)).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn error_event_oversized_input_snapshot() {
        let cell = new_error_event(
            "Message exceeds the maximum length of 1048576 characters (1048577 provided)."
                .to_string(),
        );
        let rendered = render_lines(&cell.display_lines(120)).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[tokio::test]
    async fn mcp_tools_output_masks_sensitive_values() {
        let mut config = test_config().await;
        let mut env = HashMap::new();
        env.insert("TOKEN".to_string(), "secret".to_string());
        let stdio_config = McpServerConfig {
            transport: McpServerTransportConfig::Stdio {
                command: "docs-server".to_string(),
                args: vec![],
                env: Some(env),
                env_vars: vec!["APP_TOKEN".to_string()],
                cwd: None,
            },
            enabled: true,
            required: false,
            disabled_reason: None,
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            enabled_tools: None,
            disabled_tools: None,
            scopes: None,
            oauth_resource: None,
            r#type: None,
            oauth: None,
        };
        let mut servers = config.mcp_servers.get().clone();
        servers.insert("docs".to_string(), stdio_config);

        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer secret".to_string());
        let mut env_headers = HashMap::new();
        env_headers.insert("X-API-Key".to_string(), "API_KEY_ENV".to_string());
        let http_config = McpServerConfig {
            transport: McpServerTransportConfig::StreamableHttp {
                url: "https://example.com/mcp".to_string(),
                bearer_token_env_var: Some("MCP_TOKEN".to_string()),
                http_headers: Some(headers),
                env_http_headers: Some(env_headers),
            },
            enabled: true,
            required: false,
            disabled_reason: None,
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            enabled_tools: None,
            disabled_tools: None,
            scopes: None,
            oauth_resource: None,
            r#type: None,
            oauth: None,
        };
        servers.insert("http".to_string(), http_config);
        config
            .mcp_servers
            .set(servers)
            .expect("test mcp servers should accept any configuration");

        let mut tools: HashMap<String, Tool> = HashMap::new();
        tools.insert(
            "mcp__docs__list".to_string(),
            Tool {
                description: None,
                name: "list".to_string(),
                title: None,
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
                output_schema: None,
                annotations: None,
                icons: None,
                meta: None,
            },
        );
        tools.insert(
            "mcp__http__ping".to_string(),
            Tool {
                description: None,
                name: "ping".to_string(),
                title: None,
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
                output_schema: None,
                annotations: None,
                icons: None,
                meta: None,
            },
        );

        let auth_statuses: HashMap<String, McpAuthStatus> = HashMap::new();
        let cell = new_mcp_tools_output(
            &config,
            tools,
            HashMap::new(),
            HashMap::new(),
            &auth_statuses,
        );
        let rendered = render_lines(&cell.display_lines(120)).join("\n");

        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn empty_agent_message_cell_transcript() {
        let cell = AgentMessageCell::new(vec![Line::default()], false);
        assert_eq!(cell.transcript_lines(80), vec![Line::from("  ")]);
        assert_eq!(cell.desired_transcript_height(80), 1);
    }

    #[test]
    fn prefixed_wrapped_history_cell_indents_wrapped_lines() {
        let summary = Line::from(vec![
            "You ".into(),
            "approved".bold(),
            " chaos to run ".into(),
            "echo something really long to ensure wrapping happens".dim(),
            " this time".bold(),
        ]);
        let cell = PrefixedWrappedHistoryCell::new(summary, "✔ ".green(), "  ");
        let rendered = render_lines(&cell.display_lines(24));
        assert_eq!(
            rendered,
            vec![
                "✔ You approved chaos to".to_string(),
                "  run echo something".to_string(),
                "  really long to ensure".to_string(),
                "  wrapping happens this".to_string(),
                "  time".to_string(),
            ]
        );
    }

    #[test]
    fn prefixed_wrapped_history_cell_does_not_split_url_like_token() {
        let url_like =
            "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890";
        let cell = PrefixedWrappedHistoryCell::new(Line::from(url_like), "✔ ".green(), "  ");
        let rendered = render_lines(&cell.display_lines(24));

        assert_eq!(
            rendered
                .iter()
                .filter(|line| line.contains(url_like))
                .count(),
            1,
            "expected full URL-like token in one rendered line, got: {rendered:?}"
        );
    }

    #[test]
    fn unified_exec_interaction_cell_does_not_split_url_like_stdin_token() {
        let url_like =
            "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890";
        let cell = UnifiedExecInteractionCell::new(Some("true".to_string()), url_like.to_string());
        let rendered = render_lines(&cell.display_lines(24));

        assert_eq!(
            rendered
                .iter()
                .filter(|line| line.contains(url_like))
                .count(),
            1,
            "expected full URL-like token in one rendered line, got: {rendered:?}"
        );
    }

    #[test]
    fn prefixed_wrapped_history_cell_height_matches_wrapped_rendering() {
        let url_like = "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890/artifacts/reports/performance/summary/detail/with/a/very/long/path";
        let cell: Box<dyn HistoryCell> = Box::new(PrefixedWrappedHistoryCell::new(
            Line::from(url_like),
            "✔ ".green(),
            "  ",
        ));

        let width: u16 = 24;
        let logical_height = cell.display_lines(width).len() as u16;
        let wrapped_height = cell.desired_height(width);
        assert!(
            wrapped_height > logical_height,
            "expected wrapped height to exceed logical line count ({logical_height}), got {wrapped_height}"
        );

        let area = Rect::new(0, 0, width, wrapped_height);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        cell.render(area, &mut buf);

        let first_row = (0..area.width)
            .map(|x| {
                let symbol = buf[(x, 0)].symbol();
                if symbol.is_empty() {
                    ' '
                } else {
                    symbol.chars().next().unwrap_or(' ')
                }
            })
            .collect::<String>();
        assert!(
            first_row.contains("✔"),
            "expected first rendered row to keep the prefix visible, got: {first_row:?}"
        );
    }

    #[test]
    fn unified_exec_interaction_cell_height_matches_wrapped_rendering() {
        let url_like = "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890/artifacts/reports/performance/summary/detail/with/a/very/long/path";
        let cell: Box<dyn HistoryCell> = Box::new(UnifiedExecInteractionCell::new(
            Some("true".to_string()),
            url_like.to_string(),
        ));

        let width: u16 = 24;
        let logical_height = cell.display_lines(width).len() as u16;
        let wrapped_height = cell.desired_height(width);
        assert!(
            wrapped_height > logical_height,
            "expected wrapped height to exceed logical line count ({logical_height}), got {wrapped_height}"
        );

        let area = Rect::new(0, 0, width, wrapped_height);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        cell.render(area, &mut buf);

        let first_row = (0..area.width)
            .map(|x| {
                let symbol = buf[(x, 0)].symbol();
                if symbol.is_empty() {
                    ' '
                } else {
                    symbol.chars().next().unwrap_or(' ')
                }
            })
            .collect::<String>();
        assert!(
            first_row.contains("Interacted with"),
            "expected first rendered row to keep the header visible, got: {first_row:?}"
        );
    }

    #[test]
    fn web_search_history_cell_snapshot() {
        let query =
            "example search query with several generic words to exercise wrapping".to_string();
        let cell = new_web_search_call(
            "call-1".to_string(),
            query.clone(),
            WebSearchAction::Search {
                query: Some(query),
                queries: None,
            },
        );
        let rendered = render_lines(&cell.display_lines(64)).join("\n");

        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn web_search_history_cell_wraps_with_indented_continuation() {
        let query =
            "example search query with several generic words to exercise wrapping".to_string();
        let cell = new_web_search_call(
            "call-1".to_string(),
            query.clone(),
            WebSearchAction::Search {
                query: Some(query),
                queries: None,
            },
        );
        let rendered = render_lines(&cell.display_lines(64));

        assert_eq!(
            rendered,
            vec![
                "• Searched example search query with several generic words to".to_string(),
                "  exercise wrapping".to_string(),
            ]
        );
    }

    #[test]
    fn web_search_history_cell_short_query_does_not_wrap() {
        let query = "short query".to_string();
        let cell = new_web_search_call(
            "call-1".to_string(),
            query.clone(),
            WebSearchAction::Search {
                query: Some(query),
                queries: None,
            },
        );
        let rendered = render_lines(&cell.display_lines(64));

        assert_eq!(rendered, vec!["• Searched short query".to_string()]);
    }

    #[test]
    fn web_search_history_cell_transcript_snapshot() {
        let query =
            "example search query with several generic words to exercise wrapping".to_string();
        let cell = new_web_search_call(
            "call-1".to_string(),
            query.clone(),
            WebSearchAction::Search {
                query: Some(query),
                queries: None,
            },
        );
        let rendered = render_lines(&cell.transcript_lines(64)).join("\n");

        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn active_mcp_tool_call_snapshot() {
        let invocation = McpInvocation {
            server: "search".into(),
            tool: "find_docs".into(),
            arguments: Some(json!({
                "query": "ratatui styling",
                "limit": 3,
            })),
        };

        let cell = new_active_mcp_tool_call("call-1".into(), invocation, true);
        let rendered = render_lines(&cell.display_lines(80)).join("\n");

        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn completed_mcp_tool_call_success_snapshot() {
        let invocation = McpInvocation {
            server: "search".into(),
            tool: "find_docs".into(),
            arguments: Some(json!({
                "query": "ratatui styling",
                "limit": 3,
            })),
        };

        let result = CallToolResult {
            content: vec![text_block("Found styling guidance in styles.md")],
            is_error: None,
            structured_content: None,
            meta: None,
        };

        let mut cell = new_active_mcp_tool_call("call-2".into(), invocation, true);
        assert!(
            cell.complete(Duration::from_millis(1420), Ok(result))
                .is_none()
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");

        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn completed_mcp_tool_call_image_after_text_returns_extra_cell() {
        let invocation = McpInvocation {
            server: "image".into(),
            tool: "generate".into(),
            arguments: Some(json!({
                "prompt": "tiny image",
            })),
        };

        let result = CallToolResult {
            content: vec![
                text_block("Here is the image:"),
                image_block(SMALL_PNG_BASE64),
            ],
            is_error: None,
            structured_content: None,
            meta: None,
        };

        let mut cell = new_active_mcp_tool_call("call-image".into(), invocation, true);
        let extra_cell = cell
            .complete(Duration::from_millis(25), Ok(result))
            .expect("expected image cell");

        let rendered = render_lines(&extra_cell.display_lines(80));
        assert_eq!(rendered, vec!["tool result (image output)"]);
    }

    #[test]
    fn completed_mcp_tool_call_accepts_data_url_image_blocks() {
        let invocation = McpInvocation {
            server: "image".into(),
            tool: "generate".into(),
            arguments: Some(json!({
                "prompt": "tiny image",
            })),
        };

        let data_url = format!("data:image/png;base64,{SMALL_PNG_BASE64}");
        let result = CallToolResult {
            content: vec![image_block(&data_url)],
            is_error: None,
            structured_content: None,
            meta: None,
        };

        let mut cell = new_active_mcp_tool_call("call-image-data-url".into(), invocation, true);
        let extra_cell = cell
            .complete(Duration::from_millis(25), Ok(result))
            .expect("expected image cell");

        let rendered = render_lines(&extra_cell.display_lines(80));
        assert_eq!(rendered, vec!["tool result (image output)"]);
    }

    #[test]
    fn completed_mcp_tool_call_skips_invalid_image_blocks() {
        let invocation = McpInvocation {
            server: "image".into(),
            tool: "generate".into(),
            arguments: Some(json!({
                "prompt": "tiny image",
            })),
        };

        let result = CallToolResult {
            content: vec![image_block("not-base64"), image_block(SMALL_PNG_BASE64)],
            is_error: None,
            structured_content: None,
            meta: None,
        };

        let mut cell = new_active_mcp_tool_call("call-image-2".into(), invocation, true);
        let extra_cell = cell
            .complete(Duration::from_millis(25), Ok(result))
            .expect("expected image cell");

        let rendered = render_lines(&extra_cell.display_lines(80));
        assert_eq!(rendered, vec!["tool result (image output)"]);
    }

    #[test]
    fn completed_mcp_tool_call_error_snapshot() {
        let invocation = McpInvocation {
            server: "search".into(),
            tool: "find_docs".into(),
            arguments: Some(json!({
                "query": "ratatui styling",
                "limit": 3,
            })),
        };

        let mut cell = new_active_mcp_tool_call("call-3".into(), invocation, true);
        assert!(
            cell.complete(Duration::from_secs(2), Err("network timeout".into()))
                .is_none()
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");

        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn completed_mcp_tool_call_multiple_outputs_snapshot() {
        let invocation = McpInvocation {
            server: "search".into(),
            tool: "find_docs".into(),
            arguments: Some(json!({
                "query": "ratatui styling",
                "limit": 3,
            })),
        };

        let result = CallToolResult {
            content: vec![
                text_block(
                    "Found styling guidance in styles.md and additional notes in CONTRIBUTING.md.",
                ),
                resource_link_block(
                    "file:///docs/styles.md",
                    "styles.md",
                    Some("Styles"),
                    Some("Link to styles documentation"),
                ),
            ],
            is_error: None,
            structured_content: None,
            meta: None,
        };

        let mut cell = new_active_mcp_tool_call("call-4".into(), invocation, true);
        assert!(
            cell.complete(Duration::from_millis(640), Ok(result))
                .is_none()
        );

        let rendered = render_lines(&cell.display_lines(48)).join("\n");

        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn completed_mcp_tool_call_wrapped_outputs_snapshot() {
        let invocation = McpInvocation {
            server: "metrics".into(),
            tool: "get_nearby_metric".into(),
            arguments: Some(json!({
                "query": "very_long_query_that_needs_wrapping_to_display_properly_in_the_history",
                "limit": 1,
            })),
        };

        let result = CallToolResult {
            content: vec![text_block(
                "Line one of the response, which is quite long and needs wrapping.\nLine two continues the response with more detail.",
            )],
            is_error: None,
            structured_content: None,
            meta: None,
        };

        let mut cell = new_active_mcp_tool_call("call-5".into(), invocation, true);
        assert!(
            cell.complete(Duration::from_millis(1280), Ok(result))
                .is_none()
        );

        let rendered = render_lines(&cell.display_lines(40)).join("\n");

        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn completed_mcp_tool_call_multiple_outputs_inline_snapshot() {
        let invocation = McpInvocation {
            server: "metrics".into(),
            tool: "summary".into(),
            arguments: Some(json!({
                "metric": "trace.latency",
                "window": "15m",
            })),
        };

        let result = CallToolResult {
            content: vec![
                text_block("Latency summary: p50=120ms, p95=480ms."),
                text_block("No anomalies detected."),
            ],
            is_error: None,
            structured_content: None,
            meta: None,
        };

        let mut cell = new_active_mcp_tool_call("call-6".into(), invocation, true);
        assert!(
            cell.complete(Duration::from_millis(320), Ok(result))
                .is_none()
        );

        let rendered = render_lines(&cell.display_lines(120)).join("\n");

        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn coalesces_sequential_reads_within_one_call() {
        let call_id = "c1".to_string();
        let mut cell = ExecCell::new(
            ExecCall {
                call_id: call_id.clone(),
                command: vec!["bash".into(), "-lc".into(), "echo".into()],
                parsed: vec![
                    ParsedCommand::Search {
                        query: Some("shimmer_spans".into()),
                        path: None,
                        cmd: "rg shimmer_spans".into(),
                    },
                    ParsedCommand::Read {
                        name: "shimmer.rs".into(),
                        cmd: "cat shimmer.rs".into(),
                        path: "shimmer.rs".into(),
                    },
                    ParsedCommand::Read {
                        name: "status_indicator_widget.rs".into(),
                        cmd: "cat status_indicator_widget.rs".into(),
                        path: "status_indicator_widget.rs".into(),
                    },
                ],
                output: None,
                source: ExecCommandSource::Agent,
                start_time: Some(Instant::now()),
                duration: None,
                interaction_input: None,
            },
            true,
        );
        cell.complete_call(&call_id, CommandOutput::default(), Duration::from_millis(1));

        let lines = cell.display_lines(80);
        let rendered = render_lines(&lines).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn coalesces_reads_across_multiple_calls() {
        let mut cell = ExecCell::new(
            ExecCall {
                call_id: "c1".to_string(),
                command: vec!["bash".into(), "-lc".into(), "echo".into()],
                parsed: vec![ParsedCommand::Search {
                    query: Some("shimmer_spans".into()),
                    path: None,
                    cmd: "rg shimmer_spans".into(),
                }],
                output: None,
                source: ExecCommandSource::Agent,
                start_time: Some(Instant::now()),
                duration: None,
                interaction_input: None,
            },
            true,
        );
        cell.complete_call("c1", CommandOutput::default(), Duration::from_millis(1));
        cell = cell
            .with_added_call(
                "c2".into(),
                vec!["bash".into(), "-lc".into(), "echo".into()],
                vec![ParsedCommand::Read {
                    name: "shimmer.rs".into(),
                    cmd: "cat shimmer.rs".into(),
                    path: "shimmer.rs".into(),
                }],
                ExecCommandSource::Agent,
                None,
            )
            .unwrap();
        cell.complete_call("c2", CommandOutput::default(), Duration::from_millis(1));
        cell = cell
            .with_added_call(
                "c3".into(),
                vec!["bash".into(), "-lc".into(), "echo".into()],
                vec![ParsedCommand::Read {
                    name: "status_indicator_widget.rs".into(),
                    cmd: "cat status_indicator_widget.rs".into(),
                    path: "status_indicator_widget.rs".into(),
                }],
                ExecCommandSource::Agent,
                None,
            )
            .unwrap();
        cell.complete_call("c3", CommandOutput::default(), Duration::from_millis(1));

        let lines = cell.display_lines(80);
        let rendered = render_lines(&lines).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn coalesced_reads_dedupe_names() {
        let mut cell = ExecCell::new(
            ExecCall {
                call_id: "c1".to_string(),
                command: vec!["bash".into(), "-lc".into(), "echo".into()],
                parsed: vec![
                    ParsedCommand::Read {
                        name: "auth.rs".into(),
                        cmd: "cat auth.rs".into(),
                        path: "auth.rs".into(),
                    },
                    ParsedCommand::Read {
                        name: "auth.rs".into(),
                        cmd: "cat auth.rs".into(),
                        path: "auth.rs".into(),
                    },
                    ParsedCommand::Read {
                        name: "shimmer.rs".into(),
                        cmd: "cat shimmer.rs".into(),
                        path: "shimmer.rs".into(),
                    },
                ],
                output: None,
                source: ExecCommandSource::Agent,
                start_time: Some(Instant::now()),
                duration: None,
                interaction_input: None,
            },
            true,
        );
        cell.complete_call("c1", CommandOutput::default(), Duration::from_millis(1));
        let lines = cell.display_lines(80);
        let rendered = render_lines(&lines).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn multiline_command_wraps_with_extra_indent_on_subsequent_lines() {
        let cmd = "set -o pipefail\ncargo test --all-features --quiet".to_string();
        let call_id = "c1".to_string();
        let mut cell = ExecCell::new(
            ExecCall {
                call_id: call_id.clone(),
                command: vec!["bash".into(), "-lc".into(), cmd],
                parsed: Vec::new(),
                output: None,
                source: ExecCommandSource::Agent,
                start_time: Some(Instant::now()),
                duration: None,
                interaction_input: None,
            },
            true,
        );
        cell.complete_call(&call_id, CommandOutput::default(), Duration::from_millis(1));

        let width: u16 = 28;
        let lines = cell.display_lines(width);
        let rendered = render_lines(&lines).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn single_line_command_compact_when_fits() {
        let call_id = "c1".to_string();
        let mut cell = ExecCell::new(
            ExecCall {
                call_id: call_id.clone(),
                command: vec!["echo".into(), "ok".into()],
                parsed: Vec::new(),
                output: None,
                source: ExecCommandSource::Agent,
                start_time: Some(Instant::now()),
                duration: None,
                interaction_input: None,
            },
            true,
        );
        cell.complete_call(&call_id, CommandOutput::default(), Duration::from_millis(1));
        let lines = cell.display_lines(80);
        let rendered = render_lines(&lines).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn single_line_command_wraps_with_four_space_continuation() {
        let call_id = "c1".to_string();
        let long = "a_very_long_token_without_spaces_to_force_wrapping".to_string();
        let mut cell = ExecCell::new(
            ExecCall {
                call_id: call_id.clone(),
                command: vec!["bash".into(), "-lc".into(), long],
                parsed: Vec::new(),
                output: None,
                source: ExecCommandSource::Agent,
                start_time: Some(Instant::now()),
                duration: None,
                interaction_input: None,
            },
            true,
        );
        cell.complete_call(&call_id, CommandOutput::default(), Duration::from_millis(1));
        let lines = cell.display_lines(24);
        let rendered = render_lines(&lines).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn multiline_command_without_wrap_uses_branch_then_eight_spaces() {
        let call_id = "c1".to_string();
        let cmd = "echo one\necho two".to_string();
        let mut cell = ExecCell::new(
            ExecCall {
                call_id: call_id.clone(),
                command: vec!["bash".into(), "-lc".into(), cmd],
                parsed: Vec::new(),
                output: None,
                source: ExecCommandSource::Agent,
                start_time: Some(Instant::now()),
                duration: None,
                interaction_input: None,
            },
            true,
        );
        cell.complete_call(&call_id, CommandOutput::default(), Duration::from_millis(1));
        let lines = cell.display_lines(80);
        let rendered = render_lines(&lines).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn multiline_command_both_lines_wrap_with_correct_prefixes() {
        let call_id = "c1".to_string();
        let cmd = "first_token_is_long_enough_to_wrap\nsecond_token_is_also_long_enough_to_wrap"
            .to_string();
        let mut cell = ExecCell::new(
            ExecCall {
                call_id: call_id.clone(),
                command: vec!["bash".into(), "-lc".into(), cmd],
                parsed: Vec::new(),
                output: None,
                source: ExecCommandSource::Agent,
                start_time: Some(Instant::now()),
                duration: None,
                interaction_input: None,
            },
            true,
        );
        cell.complete_call(&call_id, CommandOutput::default(), Duration::from_millis(1));
        let lines = cell.display_lines(28);
        let rendered = render_lines(&lines).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn stderr_tail_more_than_five_lines_snapshot() {
        let call_id = "c_err".to_string();
        let mut cell = ExecCell::new(
            ExecCall {
                call_id: call_id.clone(),
                command: vec!["bash".into(), "-lc".into(), "seq 1 10 1>&2 && false".into()],
                parsed: Vec::new(),
                output: None,
                source: ExecCommandSource::Agent,
                start_time: Some(Instant::now()),
                duration: None,
                interaction_input: None,
            },
            true,
        );
        let stderr: String = (1..=10)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        cell.complete_call(
            &call_id,
            CommandOutput {
                exit_code: 1,
                formatted_output: String::new(),
                aggregated_output: stderr,
            },
            Duration::from_millis(1),
        );

        let rendered = cell
            .display_lines(80)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn ran_cell_multiline_with_stderr_snapshot() {
        let call_id = "c_wrap_err".to_string();
        let long_cmd =
            "echo this_is_a_very_long_single_token_that_will_wrap_across_the_available_width";
        let mut cell = ExecCell::new(
            ExecCall {
                call_id: call_id.clone(),
                command: vec!["bash".into(), "-lc".into(), long_cmd.to_string()],
                parsed: Vec::new(),
                output: None,
                source: ExecCommandSource::Agent,
                start_time: Some(Instant::now()),
                duration: None,
                interaction_input: None,
            },
            true,
        );

        let stderr = "error: first line on stderr\nerror: second line on stderr".to_string();
        cell.complete_call(
            &call_id,
            CommandOutput {
                exit_code: 1,
                formatted_output: String::new(),
                aggregated_output: stderr,
            },
            Duration::from_millis(5),
        );

        let width: u16 = 28;
        let rendered = cell
            .display_lines(width)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn user_history_cell_wraps_and_prefixes_each_line_snapshot() {
        let msg = "one two three four five six seven";
        let cell = UserHistoryCell {
            message: msg.to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        };

        let width: u16 = 12;
        let lines = cell.display_lines(width);
        let rendered = render_lines(&lines).join("\n");

        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn user_history_cell_renders_remote_image_urls() {
        let cell = UserHistoryCell {
            message: "describe these".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: vec!["https://example.com/example.png".to_string()],
        };

        let rendered = render_lines(&cell.display_lines(80)).join("\n");

        assert!(rendered.contains("[Image #1]"));
        assert!(rendered.contains("describe these"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn user_history_cell_summarizes_inline_data_urls() {
        let cell = UserHistoryCell {
            message: "describe inline image".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: vec!["data:image/png;base64,aGVsbG8=".to_string()],
        };

        let rendered = render_lines(&cell.display_lines(80)).join("\n");

        assert!(rendered.contains("[Image #1]"));
        assert!(rendered.contains("describe inline image"));
    }

    #[test]
    fn user_history_cell_numbers_multiple_remote_images() {
        let cell = UserHistoryCell {
            message: "describe both".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: vec![
                "https://example.com/one.png".to_string(),
                "https://example.com/two.png".to_string(),
            ],
        };

        let rendered = render_lines(&cell.display_lines(80)).join("\n");

        assert!(rendered.contains("[Image #1]"));
        assert!(rendered.contains("[Image #2]"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn user_history_cell_height_matches_rendered_lines_with_remote_images() {
        let cell = UserHistoryCell {
            message: "line one\nline two".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: vec![
                "https://example.com/one.png".to_string(),
                "https://example.com/two.png".to_string(),
            ],
        };

        let width = 80;
        let rendered_len: u16 = cell
            .display_lines(width)
            .len()
            .try_into()
            .unwrap_or(u16::MAX);
        assert_eq!(cell.desired_height(width), rendered_len);
        assert_eq!(cell.desired_transcript_height(width), rendered_len);
    }

    #[test]
    fn user_history_cell_trims_trailing_blank_message_lines() {
        let cell = UserHistoryCell {
            message: "line one\n\n   \n\t \n".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: vec!["https://example.com/one.png".to_string()],
        };

        let rendered = render_lines(&cell.display_lines(80));
        let trailing_blank_count = rendered
            .iter()
            .rev()
            .take_while(|line| line.trim().is_empty())
            .count();
        assert_eq!(trailing_blank_count, 1);
        assert!(rendered.iter().any(|line| line.contains("line one")));
    }

    #[test]
    fn user_history_cell_trims_trailing_blank_message_lines_with_text_elements() {
        use chaos_ipc::user_input::TextElement;
        let message = "tokenized\n\n\n".to_string();
        let cell = UserHistoryCell {
            message,
            text_elements: vec![TextElement::new(
                (0..8).into(),
                Some("tokenized".to_string()),
            )],
            local_image_paths: Vec::new(),
            remote_image_urls: vec!["https://example.com/one.png".to_string()],
        };

        let rendered = render_lines(&cell.display_lines(80));
        let trailing_blank_count = rendered
            .iter()
            .rev()
            .take_while(|line| line.trim().is_empty())
            .count();
        assert_eq!(trailing_blank_count, 1);
        assert!(rendered.iter().any(|line| line.contains("tokenized")));
    }

    #[test]
    fn render_uses_wrapping_for_long_url_like_line() {
        let url = "https://example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890/artifacts/reports/performance/summary/detail/with/a/very/long/path/that/keeps/going/for/testing/purposes-only-and-does/not/need/to/resolve/index.html?session_id=abc123def456ghi789jkl012mno345pqr678stu901vwx234yz";
        let cell: Box<dyn HistoryCell> = Box::new(UserHistoryCell {
            message: url.to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        });

        let width: u16 = 52;
        let height = cell.desired_height(width);
        assert!(
            height > 1,
            "expected wrapped height for long URL, got {height}"
        );

        let area = Rect::new(0, 0, width, height);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        cell.render(area, &mut buf);

        let rendered = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| {
                        let symbol = buf[(x, y)].symbol();
                        if symbol.is_empty() {
                            ' '
                        } else {
                            symbol.chars().next().unwrap_or(' ')
                        }
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let rendered_blob = rendered.join("\n");

        assert!(
            rendered_blob.contains("session_id=abc123"),
            "expected URL tail to be visible after wrapping, got:\n{rendered_blob}"
        );

        let non_empty_rows = rendered.iter().filter(|row| !row.trim().is_empty()).count() as u16;
        assert!(
            non_empty_rows > 3,
            "expected long URL to span multiple visible rows, got:\n{rendered_blob}"
        );
    }

    #[test]
    fn plan_update_with_note_and_wrapping_snapshot() {
        use chaos_ipc::plan_tool::PlanItemArg;
        use chaos_ipc::plan_tool::StepStatus;
        use chaos_ipc::plan_tool::UpdatePlanArgs;

        let update = UpdatePlanArgs {
            explanation: Some(
                "I'll update Grafana call error handling by adding retries and clearer messages when the backend is unreachable."
                    .to_string(),
            ),
            plan: vec![
                PlanItemArg {
                    step: "Investigate existing error paths and logging around HTTP timeouts".into(),
                    status: StepStatus::Completed,
                },
                PlanItemArg {
                    step: "Harden Grafana client error handling with retry/backoff and user‑friendly messages".into(),
                    status: StepStatus::InProgress,
                },
                PlanItemArg {
                    step: "Add tests for transient failure scenarios and surfacing to the UI".into(),
                    status: StepStatus::Pending,
                },
            ],
        };

        let cell = new_plan_update(update);
        let lines = cell.display_lines(32);
        let rendered = render_lines(&lines).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn plan_update_without_note_snapshot() {
        use chaos_ipc::plan_tool::PlanItemArg;
        use chaos_ipc::plan_tool::StepStatus;
        use chaos_ipc::plan_tool::UpdatePlanArgs;

        let update = UpdatePlanArgs {
            explanation: None,
            plan: vec![
                PlanItemArg {
                    step: "Define error taxonomy".into(),
                    status: StepStatus::InProgress,
                },
                PlanItemArg {
                    step: "Implement mapping to user messages".into(),
                    status: StepStatus::Pending,
                },
            ],
        };

        let cell = new_plan_update(update);
        let lines = cell.display_lines(40);
        let rendered = render_lines(&lines).join("\n");
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn plan_update_does_not_split_url_like_tokens_in_note_or_step() {
        use chaos_ipc::plan_tool::PlanItemArg;
        use chaos_ipc::plan_tool::StepStatus;
        use chaos_ipc::plan_tool::UpdatePlanArgs;

        let note_url =
            "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890";
        let step_url = "example.test/api/v1/projects/beta-team/releases/2026-02-17/builds/0987654321/artifacts/reports/performance";
        let update = UpdatePlanArgs {
            explanation: Some(format!(
                "Investigate failures under {note_url} immediately."
            )),
            plan: vec![PlanItemArg {
                step: format!("Validate callbacks under {step_url} before rollout."),
                status: StepStatus::InProgress,
            }],
        };

        let cell = new_plan_update(update);
        let rendered = render_lines(&cell.display_lines(30));

        assert_eq!(
            rendered
                .iter()
                .filter(|line| line.contains(note_url))
                .count(),
            1,
            "expected full note URL-like token in one rendered line, got: {rendered:?}"
        );
        assert_eq!(
            rendered
                .iter()
                .filter(|line| line.contains(step_url))
                .count(),
            1,
            "expected full step URL-like token in one rendered line, got: {rendered:?}"
        );
    }

    #[test]
    fn reasoning_summary_block() {
        let cell = new_reasoning_summary_block(
            "**High level reasoning**\n\nDetailed reasoning goes here.".to_string(),
            &test_cwd(),
        );

        let rendered_display = render_lines(&cell.display_lines(80));
        assert_eq!(rendered_display, vec!["• Detailed reasoning goes here."]);

        let rendered_transcript = render_transcript(cell.as_ref());
        assert_eq!(rendered_transcript, vec!["• Detailed reasoning goes here."]);
    }

    #[test]
    fn reasoning_summary_height_matches_wrapped_rendering_for_url_like_content() {
        let summary = "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890/artifacts/reports/performance/summary/detail/with/a/very/long/path/that/keeps/going";
        let cell: Box<dyn HistoryCell> = Box::new(ReasoningSummaryCell::new(
            "High level reasoning".to_string(),
            summary.to_string(),
            &test_cwd(),
            false,
        ));
        let width: u16 = 24;

        let logical_height = cell.display_lines(width).len() as u16;
        let wrapped_height = cell.desired_height(width);
        let expected_wrapped_height = Paragraph::new(Text::from(cell.display_lines(width)))
            .wrap(Wrap { trim: false })
            .line_count(width) as u16;
        assert_eq!(wrapped_height, expected_wrapped_height);
        assert!(
            wrapped_height >= logical_height,
            "expected wrapped height to be at least logical line count ({logical_height}), got {wrapped_height}"
        );

        let wrapped_transcript_height = cell.desired_transcript_height(width);
        assert_eq!(wrapped_transcript_height, wrapped_height);

        let area = Rect::new(0, 0, width, wrapped_height);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        cell.render(area, &mut buf);

        let first_row = (0..area.width)
            .map(|x| {
                let symbol = buf[(x, 0)].symbol();
                if symbol.is_empty() {
                    ' '
                } else {
                    symbol.chars().next().unwrap_or(' ')
                }
            })
            .collect::<String>();
        assert!(
            first_row.contains("•"),
            "expected first rendered row to keep summary bullet visible, got: {first_row:?}"
        );
    }

    #[test]
    fn reasoning_summary_block_returns_reasoning_cell_when_feature_disabled() {
        let cell =
            new_reasoning_summary_block("Detailed reasoning goes here.".to_string(), &test_cwd());

        let rendered = render_transcript(cell.as_ref());
        assert_eq!(rendered, vec!["• Detailed reasoning goes here."]);
    }

    #[tokio::test]
    async fn reasoning_summary_block_respects_config_overrides() {
        let mut config = test_config().await;
        config.model = Some("gpt-3.5-turbo".to_string());
        config.model_supports_reasoning_summaries = Some(true);
        let cell = new_reasoning_summary_block(
            "**High level reasoning**\n\nDetailed reasoning goes here.".to_string(),
            &test_cwd(),
        );

        let rendered_display = render_lines(&cell.display_lines(80));
        assert_eq!(rendered_display, vec!["• Detailed reasoning goes here."]);
    }

    #[test]
    fn reasoning_summary_block_falls_back_when_header_is_missing() {
        let cell = new_reasoning_summary_block(
            "**High level reasoning without closing".to_string(),
            &test_cwd(),
        );

        let rendered = render_transcript(cell.as_ref());
        assert_eq!(rendered, vec!["• **High level reasoning without closing"]);
    }

    #[test]
    fn reasoning_summary_block_falls_back_when_summary_is_missing() {
        let cell = new_reasoning_summary_block(
            "**High level reasoning without closing**".to_string(),
            &test_cwd(),
        );

        let rendered = render_transcript(cell.as_ref());
        assert_eq!(rendered, vec!["• High level reasoning without closing"]);

        let cell = new_reasoning_summary_block(
            "**High level reasoning without closing**\n\n  ".to_string(),
            &test_cwd(),
        );

        let rendered = render_transcript(cell.as_ref());
        assert_eq!(rendered, vec!["• High level reasoning without closing"]);
    }

    #[test]
    fn reasoning_summary_block_splits_header_and_summary_when_present() {
        let cell = new_reasoning_summary_block(
            "**High level plan**\n\nWe should fix the bug next.".to_string(),
            &test_cwd(),
        );

        let rendered_display = render_lines(&cell.display_lines(80));
        assert_eq!(rendered_display, vec!["• We should fix the bug next."]);

        let rendered_transcript = render_transcript(cell.as_ref());
        assert_eq!(rendered_transcript, vec!["• We should fix the bug next."]);
    }

    #[test]
    fn deprecation_notice_renders_summary_with_details() {
        let cell = new_deprecation_notice(
            "Feature flag `foo`".to_string(),
            Some("Use flag `bar` instead.".to_string()),
        );
        let lines = cell.display_lines(80);
        let rendered = render_lines(&lines);
        assert_eq!(
            rendered,
            vec![
                "⚠ Feature flag `foo`".to_string(),
                "Use flag `bar` instead.".to_string(),
            ]
        );
    }
}
