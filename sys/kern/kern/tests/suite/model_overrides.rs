use chaos_ipc::openai_models::ReasoningEffort;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_test_fixtures::TEST_MODEL;
use core_test_support::responses::start_mock_server;
use core_test_support::test_chaos::test_chaos;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use std::path::Path;

async fn settings_snapshot(home: &Path) -> anyhow::Result<(i64, serde_json::Value)> {
    let runtime = chaos_kern::user_settings::open(home).await?;
    let snapshot = runtime.settings_snapshot().await?;
    Ok((snapshot.revision, snapshot.settings))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn override_turn_context_does_not_persist_when_settings_exist() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let home = std::sync::Arc::new(tempfile::TempDir::new().expect("create chaos home"));
    core_test_support::seed_settings(&home, &format!("model = \"{TEST_MODEL}\"\n")).await;
    let (initial_revision, initial_settings) = settings_snapshot(home.path()).await?;
    assert_eq!(initial_settings["model"], serde_json::json!(TEST_MODEL));

    let mut builder = test_chaos().with_home(home.clone()).with_config(|config| {
        config.model = Some(TEST_MODEL.to_string());
    });
    let test = builder.build(&server).await.expect("create conversation");
    let chaos = test.process.clone();

    chaos
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            approvals_reviewer: None,
            sandbox_policy: None,

            model: Some("o3".to_string()),
            effort: Some(Some(ReasoningEffort::High)),
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await
        .expect("submit override");

    chaos.submit(Op::Shutdown).await.expect("request shutdown");
    wait_for_event(&chaos, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;

    let (revision, settings) = settings_snapshot(home.path()).await?;
    assert_eq!(
        revision, initial_revision,
        "override must not commit settings"
    );
    assert_eq!(settings, initial_settings);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn override_turn_context_does_not_create_settings() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_chaos();
    let test = builder.build(&server).await.expect("create conversation");
    let chaos = test.process.clone();
    let home = test.home.clone();
    let bootstrap =
        chaos_kern::user_settings::BootstrapConfig::read(home.path()).expect("read bootstrap");
    let (initial_revision, initial_settings) = settings_snapshot(home.path()).await?;

    chaos
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            approvals_reviewer: None,
            sandbox_policy: None,

            model: Some("o3".to_string()),
            effort: Some(Some(ReasoningEffort::Medium)),
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await
        .expect("submit override");

    chaos.submit(Op::Shutdown).await.expect("request shutdown");
    wait_for_event(&chaos, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;

    assert_eq!(
        chaos_kern::user_settings::BootstrapConfig::read(home.path()).expect("read bootstrap"),
        bootstrap,
        "override must not touch the bootstrap file"
    );
    let (revision, settings) = settings_snapshot(home.path()).await?;
    assert_eq!(
        revision, initial_revision,
        "override must not commit settings"
    );
    assert_eq!(settings, initial_settings);
    Ok(())
}
