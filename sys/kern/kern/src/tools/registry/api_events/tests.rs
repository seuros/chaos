use super::*;
use crate::activity::{Activity, Station};
use crate::tools::context::ToolInvocation;
use crate::turn_diff_tracker::TurnDiffTracker;
use std::sync::Arc;

#[tokio::test]
async fn a_console_tool_call_reaches_the_monitor_but_not_the_console() {
    let mut bus = crate::activity::subscribe();
    let (session, turn, client) = crate::chaos::make_session_and_context_with_rx().await;
    assert!(!matches!(
        session.session_source().await,
        SessionSource::Api | SessionSource::Acp
    ));
    let invocation = ToolInvocation {
        session: Arc::clone(&session),
        turn,
        tracker: Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        call_id: "call-console".into(),
        tool_name: "read_file".into(),
        tool_namespace: None,
        payload: ToolPayload::Function {
            arguments: "{}".into(),
        },
    };
    let events = ToolEvents::start(&invocation, "{}")
        .await
        .expect("reported");
    events
        .finish(&invocation, Duration::from_millis(3), true, String::new())
        .await;

    let ours = async {
        let mut seen = Vec::new();
        while seen.len() < 2 {
            let event = bus.recv().await.unwrap();
            if event.process_id == session.conversation_id {
                seen.push(event.activity);
            }
        }
        seen
    };
    let seen = tokio::time::timeout(Duration::from_secs(5), ours)
        .await
        .expect("the call never reached the bus");
    assert!(
        matches!(&seen[0], Activity::CallStarted { station: Station::Files, label, .. } if label == "read_file"),
        "{seen:?}"
    );
    assert!(matches!(&seen[1], Activity::CallEnded { .. }), "{seen:?}");
    while let Ok(event) = client.try_recv() {
        assert!(
            !matches!(
                event.msg,
                EventMsg::McpToolCallBegin(_) | EventMsg::McpToolCallEnd(_)
            ),
            "the console renders tools its own way: {event:?}"
        );
    }
}
