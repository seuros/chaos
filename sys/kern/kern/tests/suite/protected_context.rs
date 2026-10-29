//! `protected_context_files` is read once when a session is created and
//! carried in its meta record, so resume and fork restore the same text even
//! after the files change on disk.

use std::sync::Arc;

use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::RolloutItem;
use chaos_kern::RolloutRecorder;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_chaos::TestChaos;
use core_test_support::test_chaos::test_chaos;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

const V1: &str = "PROTECTED_SOUL_SNAPSHOT_V1";
const V2: &str = "PROTECTED_SOUL_SNAPSHOT_V2";
/// One environment context opens every initial context, so it counts them.
const INITIAL_CONTEXT: &str = "<environment_context>";

fn count(request: &ResponsesRequest, marker: &str) -> usize {
    request.body_json().to_string().matches(marker).count()
}

async fn turn(
    test: &TestChaos,
    server: &wiremock::MockServer,
    prompt: &str,
) -> anyhow::Result<ResponsesRequest> {
    let mock = mount_sse_once(
        server,
        sse(vec![
            ev_assistant_message("msg-1", "ok"),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    test.submit_turn(prompt).await?;
    Ok(mock.single_request())
}

async fn shutdown(test: &TestChaos) -> anyhow::Result<()> {
    test.process.submit(Op::Shutdown {}).await?;
    wait_for_event(&test.process, |event| {
        matches!(event, EventMsg::ShutdownComplete)
    })
    .await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protected_context_is_a_session_snapshot_that_resume_and_fork_restore() -> anyhow::Result<()>
{
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let files = TempDir::new()?;
    let soul = files.path().join("soul.md");
    std::fs::write(&soul, format!("{V1}\n"))?;

    let configured = soul.clone();
    let mut builder = test_chaos().with_config(move |config| {
        config.protected_context_files = vec![configured];
    });
    let test = builder.build(&server).await?;
    let first = turn(&test, &server, "first").await?;
    assert_eq!(count(&first, V1), 1, "the snapshot is sent exactly once");

    // Editing the file mid-session changes nothing the session carries.
    std::fs::write(&soul, format!("{V2}\n"))?;
    let second = turn(&test, &server, "second").await?;
    assert_eq!(count(&second, V1), 1);
    assert_eq!(count(&second, V2), 0);

    // A fork writes its own meta, carrying the inherited snapshot rather
    // than today's file.
    let parent_id = test.session_configured.session_id;
    let forked = test
        .process_table
        .fork_process_by_id(
            usize::MAX,
            test.config.clone(),
            parent_id,
            /*persist_extended_history*/ false,
            /*parent_trace*/ None,
        )
        .await?;
    let child = TestChaos {
        home: Arc::clone(&test.home),
        cwd: Arc::clone(&test.cwd),
        config: test.config.clone(),
        process: forked.process,
        session_configured: forked.session_configured,
        process_table: Arc::clone(&test.process_table),
    };
    let child_id = forked.process_id;
    let child_turn = turn(&child, &server, "child").await?;
    // The fork keeps other initial context, but replaces the inherited
    // protected section instead of duplicating it.
    assert_eq!(count(&child_turn, INITIAL_CONTEXT), 2);
    assert_eq!(count(&child_turn, V1), 1);
    assert_eq!(count(&child_turn, V2), 0);
    let child_history = RolloutRecorder::get_rollout_history_for_process(child_id).await?;
    let child_meta = child_history
        .get_rollout_items()
        .into_iter()
        .find_map(|item| match item {
            RolloutItem::SessionMeta(line) => Some(line.meta),
            _ => None,
        })
        .expect("the child records a meta");
    assert_eq!(child_meta.id, child_id, "the first meta is the child's own");
    let child_snapshot = child_meta
        .protected_context
        .expect("the child's meta carries the snapshot");
    assert_eq!(child_snapshot.files.len(), 1);
    assert_eq!(child_snapshot.files[0].text, format!("{V1}\n"));
    shutdown(&child).await?;
    shutdown(&test).await?;

    // Resuming either session from the journal restores V1, never V2.
    let home = Arc::clone(&test.home);
    for id in [parent_id, child_id] {
        let resumed = builder.resume(&server, Arc::clone(&home), id).await?;
        let request = turn(&resumed, &server, "after resume").await?;
        assert_eq!(
            count(&request, V1),
            1,
            "resume of {id} keeps exactly one protected snapshot"
        );
        assert_eq!(
            count(&request, V2),
            0,
            "resume of {id} does not re-read the file"
        );
        shutdown(&resumed).await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_protected_file_is_a_startup_warning_and_not_an_error() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let files = TempDir::new()?;
    let missing = files.path().join("absent.md");
    let mut builder = test_chaos().with_config(move |config| {
        config.protected_context_files = vec![missing];
    });
    let test = builder.build(&server).await?;
    wait_for_event(&test.process, |event| match event {
        EventMsg::Warning(warning) => warning.message.contains("absent.md"),
        _ => false,
    })
    .await;
    let request = turn(&test, &server, "hello").await?;
    assert_eq!(count(&request, "<protected_context>"), 0);
    shutdown(&test).await?;
    Ok(())
}
