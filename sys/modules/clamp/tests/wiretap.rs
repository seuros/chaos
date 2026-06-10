//! Wiretap integration test: spawn claude routed through the wiretap proxy,
//! run one turn, and assert the proxy recorded the `/v1/messages` request.
//!
//! Run with:
//!   CHAOS_CLAMP_SMOKE=1 cargo test -p chaos-clamp --test wiretap -- --ignored --nocapture

use chaos_clamp::{ClampConfig, ClampTransport, Message, WiretapProxy};

#[ignore = "requires local Claude Code CLI and authenticated environment"]
#[tokio::test]
async fn wiretap_records_turn() {
    if std::env::var_os("CHAOS_CLAMP_SMOKE").is_none() {
        eprintln!("skipping wiretap test; set CHAOS_CLAMP_SMOKE=1 to enable");
        return;
    }

    let record_file = std::env::temp_dir().join("chaos-clamp-wiretap-test.jsonl");
    let _ = std::fs::remove_file(&record_file);

    eprintln!("[test] starting wiretap proxy...");
    let proxy = WiretapProxy::start_to_file(Some(record_file.clone()))
        .await
        .expect("proxy start failed");
    let base_url = proxy.base_url();
    eprintln!(
        "[test] proxy at {base_url}, recording to {}",
        record_file.display()
    );

    // allow_claude_code_tools=true so the subprocess can answer without the
    // Chaos MCP bridge; the wiretap is what we're exercising here.
    let config = ClampConfig {
        anthropic_base_url: Some(base_url),
        allow_claude_code_tools: true,
        ..Default::default()
    };

    let mut transport = ClampTransport::spawn(config).await.expect("spawn failed");
    transport.initialize().await.expect("init failed");
    transport
        .send_user_message("Respond with exactly: WIRETAP_OK")
        .await
        .expect("send failed");

    let mut got_result = false;
    while let Ok(Some(msg)) = transport.next_message().await {
        if let Message::Result { .. } = &msg {
            got_result = true;
            break;
        }
    }
    assert!(got_result, "never received a result message");
    transport.shutdown().await.expect("shutdown failed");

    // Give the async file sink a moment to flush.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    proxy.shutdown();

    let recorded = std::fs::read_to_string(&record_file).expect("read record file");
    eprintln!("[test] recorded {} bytes", recorded.len());

    let messages_line = recorded
        .lines()
        .find(|line| line.contains("/v1/messages"))
        .expect("no /v1/messages request recorded");
    let envelope: serde_json::Value =
        serde_json::from_str(messages_line).expect("record line is valid json");

    assert_eq!(envelope["method"], "POST");
    assert_eq!(envelope["status"], 200);
    // Auth must be redacted in the record.
    if let Some(auth) = envelope["headers"].get("authorization") {
        assert_eq!(auth, "<redacted>", "authorization header must be redacted");
    }
    // The request body should carry the model + messages we sent.
    assert!(
        envelope["request"]["messages"].is_array(),
        "recorded request should include the messages array"
    );
    // The response body (SSE) should be captured by the tee.
    let response = envelope["response"]
        .as_str()
        .expect("response body should be recorded as a string");
    assert!(
        response.contains("event:") || response.contains("message_start"),
        "recorded response should look like an SSE stream, got {} bytes",
        response.len()
    );
    eprintln!("[test] captured {} bytes of response body", response.len());

    eprintln!("[test] done — wiretap captured the turn (both directions)");
}
