use super::*;
use chaos_cron::CreateJobParams;
use chaos_cron::CronStore;
use chaos_cron::Schedule;

#[tokio::test]
async fn scheduled_authorization_survives_storage_but_not_policy_revocation() {
    let (session, mut turn) = crate::chaos::make_session_and_context().await;
    let home = tempfile::tempdir().unwrap();
    let cwd = home.path().canonicalize().unwrap();
    let config_path = cwd.join("config.toml");
    std::fs::write(&config_path, "sandbox_mode = 'workspace-write'\n").unwrap();
    std::fs::create_dir(cwd.join("rules")).unwrap();
    let rules_path = cwd.join("rules/cron.decrees");
    let allowed = "prefix_rule {pattern = {'echo'}, decision = 'allow'}\n";
    std::fs::write(&rules_path, allowed).unwrap();
    crate::user_settings::migrate(&cwd, false).await.unwrap();
    crate::user_settings::put_scoped_approval(
        &cwd,
        &cwd,
        "shell",
        serde_json::json!({"prefix":["echo"]}),
    )
    .await
    .unwrap();
    let config = load_current_config(&cwd, &cwd).await.unwrap();
    turn.config = Arc::new(config.clone());
    turn.cwd = cwd.clone();
    turn.sub_id = "scheduled-policy-test".to_string();
    turn.vfs_policy = config.permissions.vfs_policy.clone();
    turn.socket_policy = config.permissions.socket_policy;
    turn.approval_policy = config.permissions.approval_policy.clone();
    turn.shell_environment_policy = config.permissions.shell_environment_policy.clone();
    session.permission_actor.register_turn(&turn).await.unwrap();

    let policy = authorize(&session, &turn, "echo cron").await.unwrap();
    let mut params = CreateJobParams::shell(
        "policy-round-trip".into(),
        Schedule::Interval { seconds: 300 }.to_json(),
        "echo cron".into(),
        CronScope::Project,
        Some(cwd.to_string_lossy().to_string()),
        None,
    );
    params.execution_policy = Some(policy);
    let pool = chaos_proc::open_runtime_db(&cwd).await.unwrap();
    let store = CronStore::new(pool.clone());
    let id = store.create(&params).await.unwrap().id;
    drop(store);
    pool.close().await;
    let store = CronStore::new(chaos_proc::open_runtime_db(&cwd).await.unwrap());
    let job = store.get(&id).await.unwrap().unwrap();
    let (saved, _) = prepare(&config, &job).await.unwrap();
    assert_eq!(
        saved.creator_session_id,
        session.conversation_id.to_string()
    );

    // A legacy job and an edited command must never reach the shell.
    let mut invalid = job.clone();
    invalid.execution_policy = None;
    assert!(
        executor(&config)(&invalid)
            .await
            .unwrap_err()
            .contains("legacy")
    );
    invalid.execution_policy = job.execution_policy.clone();
    invalid.command = "echo changed".into();
    assert!(
        executor(&config)(&invalid)
            .await
            .unwrap_err()
            .contains("authorization")
    );
    assert!(
        serde_json::from_value::<chaos_cron::tools::create::CronCreateParams>(serde_json::json!({
            "name": "injected", "schedule": {"kind": "interval", "seconds": 300},
            "command": "echo cron", "execution_policy": job.execution_policy
        }))
        .is_err()
    );

    // Fresh rules apply to both issuance and execution after reopening the
    // database. Prompt rules cannot turn into unattended auto-approval, and
    // malformed rules cannot use the interactive loader's fallback.
    for rules in [
        "prefix_rule {pattern = {'echo'}, decision = 'forbidden'}\n",
        "prefix_rule {pattern = {'echo'}, decision = 'prompt'}\n",
        "not valid rules (",
    ] {
        let runtime = crate::user_settings::open(&cwd).await.unwrap();
        let snapshot = runtime.settings_snapshot().await.unwrap();
        runtime
            .commit_settings_import(
                snapshot.revision,
                &snapshot.settings,
                None,
                &[],
                Some(&serde_json::json!([rules])),
            )
            .await
            .unwrap();
        assert!(
            authorize(&session, &turn, "echo cron").await.is_err(),
            "{rules}"
        );
        assert!(executor(&config)(&job).await.is_err(), "{rules}");
    }
    let runtime = crate::user_settings::open(&cwd).await.unwrap();
    let snapshot = runtime.settings_snapshot().await.unwrap();
    runtime
        .commit_settings_import(
            snapshot.revision,
            &snapshot.settings,
            None,
            &[],
            Some(&serde_json::json!([])),
        )
        .await
        .unwrap();

    // The launcher cannot lend a saved grant different network access.
    let mut restricted_launcher = config.clone();
    restricted_launcher.permissions.socket_policy = SocketPolicy::Enabled;
    assert!(prepare(&restricted_launcher, &job).await.is_err());
    restricted_launcher = config.clone();
    restricted_launcher
        .permissions
        .shell_environment_policy
        .inherit = crate::config::types::ShellEnvironmentPolicyInherit::None;
    assert!(prepare(&restricted_launcher, &job).await.is_err());
    let snapshot = runtime.settings_snapshot().await.unwrap();
    runtime
        .commit_settings(
            snapshot.revision,
            &serde_json::json!({"sandbox_mode":"read-only"}),
            None,
        )
        .await
        .unwrap();
    assert!(authorize(&session, &turn, "echo cron").await.is_err());
    assert!(
        executor(&config)(&job)
            .await
            .unwrap_err()
            .contains("constraints")
    );
}
