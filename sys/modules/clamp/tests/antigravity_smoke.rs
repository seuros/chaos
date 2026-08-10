//! Live Antigravity transport smoke and resume test.
//!
//! Run with:
//!   CHAOS_AGY_SMOKE=1 \
//!   CHAOS_AGY_PATH=/path/to/agy \
//!   CHAOS_AGY_HOME=/path/to/isolated/home \
//!   cargo test -p chaos-clamp --test antigravity_smoke -- --ignored --nocapture

use std::path::PathBuf;
use std::time::Duration;

use chaos_clamp::AntigravityConfig;
use chaos_clamp::AntigravityToolAuthority;
use chaos_clamp::AntigravityTransport;

#[ignore = "requires an official local agy CLI and authenticated isolated home"]
#[tokio::test]
async fn antigravity_round_trip_and_resume() {
    if std::env::var_os("CHAOS_AGY_SMOKE").is_none() {
        eprintln!("skipping Antigravity smoke test; set CHAOS_AGY_SMOKE=1 to enable");
        return;
    }

    let cli_path = std::env::var_os("CHAOS_AGY_PATH")
        .map(PathBuf::from)
        .expect("CHAOS_AGY_PATH must point to the pinned official agy binary");
    let home = std::env::var_os("CHAOS_AGY_HOME")
        .map(PathBuf::from)
        .expect("CHAOS_AGY_HOME must point to an authenticated isolated home");
    let model =
        std::env::var("CHAOS_AGY_MODEL").unwrap_or_else(|_| "gemini-3.1-pro-low".to_string());

    let config = AntigravityConfig {
        cli_path: Some(cli_path),
        home: Some(home),
        cwd: Some(std::env::current_dir().expect("current directory")),
        model,
        agent: Some("souls-house-clamp".to_string()),
        print_timeout: Duration::from_secs(120),
        ..Default::default()
    };
    let mut transport = AntigravityTransport::new(config).expect("create transport");
    assert_eq!(
        transport.tool_authority(),
        AntigravityToolAuthority::ModelOnlySandboxed
    );

    let fresh = transport
        .run_turn("Reply with exactly: CHAOS_AGY_OK")
        .await
        .expect("fresh Antigravity turn");
    assert_eq!(fresh.response.trim(), "CHAOS_AGY_OK");
    assert!(
        fresh
            .usage
            .as_ref()
            .is_some_and(|usage| usage.total_tokens > 0),
        "fresh turn should report usage"
    );

    let conversation_id = fresh.conversation_id;
    let resumed = transport
        .run_turn("What exact marker did you return previously? Reply with only that marker.")
        .await
        .expect("resumed Antigravity turn");
    assert_eq!(resumed.conversation_id, conversation_id);
    assert_eq!(resumed.response.trim(), "CHAOS_AGY_OK");
}
