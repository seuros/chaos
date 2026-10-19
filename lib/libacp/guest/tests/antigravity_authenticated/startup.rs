//! Identical-harness startup controls: no prompts, MCP servers or tool requests.
use super::*;
use std::time::Instant;

#[ignore = "no-prompt live control; explicit approved dedicated OAuth profile required"]
#[tokio::test]
async fn no_hook_control_without_prompt() {
    startup_only(false).await;
}

#[ignore = "no-prompt live hook control; compiled deny-tools hook required"]
#[tokio::test]
async fn with_hook_control_without_prompt() {
    startup_only(true).await;
}

async fn startup_only(with_hook: bool) {
    let workspace = tempfile::tempdir().expect("empty startup workspace");
    let home = PathBuf::from(std::env::var_os("CHAOS_AGY_ACP_HOME").expect("dedicated home"));
    assert!(
        !home
            .join(".gemini/config/hooks.json")
            .try_exists()
            .expect("hook metadata"),
        "refuse to run the control against an existing operator hook"
    );
    let hook = with_hook.then(|| {
        HookExperiment::install(
            HookFormat::Documented,
            HookLocation::Global,
            workspace.path(),
        )
    });
    let (agent, connection, mut updates) = connected(Arc::new(DenialAudit::default())).await;
    let client = connection.client();
    let started = Instant::now();
    let created = request_draining(
        &client,
        &mut updates,
        NewSessionRequest::new(workspace.path()),
    )
    .await;
    eprintln!(
        "Startup control: hook={with_hook}, elapsed_ms={}, \
         session_created={}, stderr_categories={:?}",
        started.elapsed().as_millis(),
        created.is_ok(),
        agent.diagnostic_categories(),
    );
    if let Err(GuestError::Protocol(error)) = &created {
        // Raw error messages/data can contain credentials: classify in memory
        // and expose only fixed labels, never format them into diagnostics.
        eprintln!(
            "Startup ACP error categories: {:?}",
            support::diagnostic_categories_for_text(&format!("{error:?}")),
        );
    }
    let closed = connection.disconnect().await;
    agent
        .stop()
        .await
        .expect("reap startup control runtime group");
    if let Some(hook) = hook.as_ref() {
        let (invoked, denied, allowed) = hook.observations();
        eprintln!(
            "Startup hook observations: invoked={invoked}, denied={denied}, allowed={allowed}",
        );
    }
    required(created, "startup control session/new");
    closed.expect("disconnect startup control");
}
