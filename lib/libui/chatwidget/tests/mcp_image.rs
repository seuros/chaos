use super::*;
use chaos_ipc::mcp::CallToolResult;
use chaos_ipc::protocol::McpInvocation;
use chaos_ipc::protocol::McpToolCallBeginEvent;
use chaos_ipc::protocol::McpToolCallEndEvent;
use ratatui::style::Color;
use std::time::Duration;

#[expect(clippy::disallowed_methods, reason = "Source image pixel assertions")]
pub(super) async fn run() {
    // Cover both live begin/end handling and an end received without a live cell.
    for with_begin in [true, false] {
        let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;
        drain_insert_history(&mut rx);
        let invocation = McpInvocation {
            server: Some("prometheus".into()),
            tool: "show_logo".into(),
            arguments: None,
        };
        if with_begin {
            chat.handle_codex_event(Event {
                id: "image".into(),
                msg: EventMsg::McpToolCallBegin(McpToolCallBeginEvent {
                    call_id: "image".into(),
                    invocation: invocation.clone(),
                }),
            });
        }
        chat.handle_codex_event(Event {
            id: "image".into(),
            msg: EventMsg::McpToolCallEnd(McpToolCallEndEvent {
                call_id: "image".into(),
                invocation,
                duration: Duration::from_millis(10),
                result: Ok(CallToolResult {
                    content: vec![
                        serde_json::json!({"type": "text", "text": "Logo caption"}),
                        serde_json::json!({
                            "type": "image",
                            "mimeType": "image/png",
                            "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
                        }),
                    ],
                    is_error: None,
                    structured_content: None,
                    meta: None,
                }),
            }),
        });
        let history = drain_insert_history(&mut rx);
        let text = history
            .iter()
            .flatten()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Logo caption"));
        assert!(text.contains("Image 1×1 · preview"));
        assert!(!text.contains("tool result (image output)"));
        assert!(!text.contains("iVBOR"));
        assert!(
            history
                .iter()
                .flatten()
                .flat_map(|line| &line.spans)
                .any(|span| span.content == "▀" && span.style.fg == Some(Color::Rgb(255, 0, 0)))
        );
        assert!(chat.active_cell.is_none());
    }
}
