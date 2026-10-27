use super::harness::PtyConsole;
use anyhow::{Context, Result, ensure};
use core_test_support::responses::{
    ev_assistant_message, ev_completed, ev_message_item_added, ev_output_text_delta,
    ev_response_created, sse,
};
use core_test_support::streaming_sse::{
    StreamingSseChunk, StreamingSseServer, start_streaming_sse_server,
};
use serde_json::Value;
use tokio::sync::oneshot;

#[cfg(target_os = "freebsd")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "root-only UI/Warden gate; requires ABI 1, explicit helper/journal and non-root user"]
async fn freebsd_warden_ui_readonly_exec_resize_and_shutdown() -> Result<()> {
    use super::warden::WardenLaunch;
    use core_test_support::responses::ev_function_call;

    let launch = WardenLaunch::from_test_environment()?;
    let command = format!(
        "set -eu; test \"$(/sbin/sysctl -n security.jail.jailed)\" = 1; \
         test \"$(/usr/bin/id -u)\" = {}; \
         if (printf forbidden > warden-readonly-canary) 2>/dev/null; then exit 90; fi; \
         printf 'WARDEN_UI_EXEC_OK\\n'",
        launch.uid
    );
    let arguments = serde_json::json!({
        "cmd": command,
        "login": false,
        "tty": false,
        "yield_time_ms": 1000,
    });
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![StreamingSseChunk {
            gate: None,
            body: sse(vec![
                ev_response_created("warden-ui-enable"),
                ev_function_call(
                    "warden-ui-enable-shell",
                    "enable_tools",
                    r#"{"groups":["shell"]}"#,
                ),
                ev_completed("warden-ui-enable"),
            ]),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: sse(vec![
                ev_response_created("warden-ui-tool"),
                ev_function_call("warden-ui-exec", "exec_command", &arguments.to_string()),
                ev_completed("warden-ui-tool"),
            ]),
        }],
        reply("warden-ui-done", "WARDEN_UI_REPLY_OK"),
    ])
    .await;
    let result = async {
        let mut console = PtyConsole::start_warden(&server, "WARDEN_UI_PROMPT", launch).await?;
        console.wait_screen("WARDEN_UI_REPLY_OK").await?;
        console.resize_and_wait(39, 101).await?;
        console.verify_warden_canary()?;
        console.shutdown().await?;
        let requests = requests(&server).await?;
        ensure!(
            requests.len() == 3,
            "expected shell activation, one execution and one reply"
        );
        let output = requests[2]["input"]
            .as_array()
            .context("missing final request input")?
            .iter()
            .find(|item| {
                item["type"] == "function_call_output" && item["call_id"] == "warden-ui-exec"
            })
            .context("missing tool result")?["output"]
            .as_str()
            .context("tool output is not text")?;
        ensure!(
            output.contains("WARDEN_UI_EXEC_OK") && output.contains("Process exited with code 0"),
            "UI tool failed confinement checks: {output}"
        );
        Ok(())
    }
    .await;
    server.shutdown().await;
    result
}

fn reply(id: &str, text: &str) -> Vec<StreamingSseChunk> {
    vec![StreamingSseChunk {
        gate: None,
        body: sse(vec![
            ev_response_created(id),
            ev_assistant_message(id, text),
            ev_completed(id),
        ]),
    }]
}

async fn requests(server: &StreamingSseServer) -> Result<Vec<Value>> {
    server
        .requests()
        .await
        .iter()
        .map(|body| serde_json::from_slice(body).map_err(Into::into))
        .collect()
}

fn last_user_text(request: &Value) -> Result<String> {
    let input = request["input"]
        .as_array()
        .context("missing request input")?;
    let message = input
        .iter()
        .rev()
        .find(|item| item["role"] == "user")
        .context("missing user message")?;
    let content = message["content"]
        .as_array()
        .context("missing user message content")?;
    Ok(content
        .iter()
        .filter_map(|item| item["text"].as_str())
        .collect())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resizing_a_live_reply_preserves_unicode_draft_and_exact_submission() -> Result<()> {
    let (finish_tx, finish_rx) = oneshot::channel();
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: sse(vec![
                    ev_response_created("stream"),
                    ev_message_item_added("stream-message", ""),
                    // A heading is stable before the message ends; trailing
                    // prose is deliberately buffered by the Markdown renderer.
                    ev_output_text_delta("# PTY_STREAM_STARTED\n\n"),
                ]),
            },
            StreamingSseChunk {
                gate: Some(finish_rx),
                body: sse(vec![
                    ev_output_text_delta("PTY_STREAM_FINISHED\n"),
                    ev_assistant_message(
                        "stream-message",
                        "# PTY_STREAM_STARTED\n\nPTY_STREAM_FINISHED\n",
                    ),
                    ev_completed("stream"),
                ]),
            },
        ],
        reply("second", "PTY_SECOND_REPLY"),
    ])
    .await;

    let result = async {
        let mut console = PtyConsole::start(&server, Some("PTY_FIRST_PROMPT")).await?;
        console.wait_screen("PTY_STREAM_STARTED").await?;
        let draft = "keep 界 🦀 e\u{301}\nsecond paragraph";
        console.paste(draft).await?;
        console.wait_screen("second paragraph").await?;

        // A storm of width and height changes while the provider is deliberately
        // paused. Completion is released by the test, never by a timing sleep.
        for _ in 0..20 {
            console.resize(18, 61).await?;
            console.resize(38, 100).await?;
        }
        console.resize_and_wait(39, 101).await?;
        console.type_text(" after resize").await?;
        console.wait_screen("second paragraph after resize").await?;
        ensure!(
            requests(&server).await?.len() == 1,
            "resizing or editing an unsent draft submitted a prompt"
        );

        finish_tx
            .send(())
            .map_err(|_| anyhow::anyhow!("provider stream closed before release"))?;
        console.wait_screen("PTY_STREAM_FINISHED").await?;
        console.wait_screen("second paragraph after resize").await?;
        console.enter().await?;
        console.wait_screen("PTY_SECOND_REPLY").await?;
        console.shutdown().await?;

        let requests = requests(&server).await?;
        ensure!(
            requests.len() == 2,
            "expected exactly two submissions: {requests:#?}"
        );
        assert_eq!(last_user_text(&requests[0])?, "PTY_FIRST_PROMPT");
        assert_eq!(
            last_user_text(&requests[1])?,
            format!("{draft} after resize")
        );
        Ok(())
    }
    .await;
    server.shutdown().await;
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_unicode_paste_survives_resize_and_expands_exactly_once() -> Result<()> {
    let (server, _completions) =
        start_streaming_sse_server(vec![reply("paste", "PTY_PASTE_REPLY")]).await;
    let result = async {
        let mut console = PtyConsole::start(&server, None).await?;
        let draft = format!("BEGIN {}\nEND", "界 🦀 e\u{301} ".repeat(300));
        let placeholder = format!("[Pasted Content {} chars]", draft.chars().count());
        console.paste(&draft).await?;
        console.wait_screen(&placeholder).await?;
        for _ in 0..10 {
            console.resize(18, 61).await?;
            console.resize(38, 100).await?;
        }
        console.resize_and_wait(39, 101).await?;
        console.type_text(" tail").await?;
        console.wait_screen(" tail").await?;
        ensure!(
            requests(&server).await?.is_empty(),
            "paste or resize submitted before Enter"
        );
        console.enter().await?;
        console.wait_screen("PTY_PASTE_REPLY").await?;
        console.shutdown().await?;

        let requests = requests(&server).await?;
        ensure!(
            requests.len() == 1,
            "paste submitted more than once: {requests:#?}"
        );
        assert_eq!(last_user_text(&requests[0])?, format!("{draft} tail"));
        Ok(())
    }
    .await;
    server.shutdown().await;
    result
}
