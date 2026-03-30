//! Smoke test: spawn claude, initialize, send one prompt, read response.
//!
//! Run with:
//!   CHAOS_CLAMP_SMOKE=1 cargo test -p chaos-clamp --test smoke -- --ignored --nocapture

use chaos_clamp::{ClampConfig, ClampTransport, Message};

#[ignore = "requires local Claude Code CLI and authenticated environment"]
#[tokio::test]
async fn clamp_round_trip() {
    if std::env::var_os("CHAOS_CLAMP_SMOKE").is_none() {
        eprintln!("skipping clamp smoke test; set CHAOS_CLAMP_SMOKE=1 to enable");
        return;
    }

    let config = ClampConfig {
        ..Default::default()
    };

    eprintln!("[test] spawning claude subprocess...");
    let mut transport = ClampTransport::spawn(config).await.expect("spawn failed");

    eprintln!("[test] initializing control protocol...");
    let init = transport.initialize().await.expect("init failed");
    eprintln!(
        "[test] initialized: {}",
        serde_json::to_string_pretty(&init).unwrap_or_default()
    );

    eprintln!("[test] sending prompt...");
    transport
        .send_user_message("Respond with exactly: CLAMP_OK")
        .await
        .expect("send failed");

    eprintln!("[test] reading messages...");
    let mut got_result = false;
    while let Ok(Some(msg)) = transport.next_message().await {
        match &msg {
            Message::Assistant { message } => {
                eprintln!("[test] assistant: {message}");
            }
            Message::Result { total_cost_usd, .. } => {
                eprintln!("[test] turn complete (cost: {total_cost_usd:?})");
                got_result = true;
                break;
            }
            Message::System { message } => {
                eprintln!("[test] system: {message}");
            }
            _ => {}
        }
    }

    assert!(got_result, "never received a result message");

    eprintln!("[test] shutting down...");
    transport.shutdown().await.expect("shutdown failed");
    eprintln!("[test] done!");
}
