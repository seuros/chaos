use super::*;
use chaos_ipc::protocol::{ApprovalPolicy, EventMsg};
use chaos_mcp_runtime::{ElicitationAction, ElicitationResponse};
use serde_json::json;
use std::sync::Arc;

fn create(id: &str, project: Option<&str>) -> HookChangeRequest {
    serde_json::from_value(json!({
        "action": "create", "id": id, "enabled": true,
        "definition": {"event": "before_turn", "command": "cat >/dev/null; printf hello", "project": project}
    }))
    .unwrap()
}

#[tokio::test]
async fn scopes_trust_and_no_file_discovery() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let other = tempfile::tempdir()?;
    std::fs::write(home.path().join("hooks.json"), "invalid legacy file")?;
    assert!(list(home.path(), project.path()).await?.is_empty());
    prepare(home.path(), project.path(), create("global", None))
        .await?
        .commit()
        .await?;
    prepare(home.path(), project.path(), create("project", Some(".")))
        .await?
        .commit()
        .await?;
    let view = list(home.path(), project.path()).await?;
    assert!(view[0].inactive_reason.is_none());
    assert_eq!(
        view[1].inactive_reason.as_deref(),
        Some("project is not trusted")
    );
    let db = crate::user_settings::open(home.path()).await?;
    db.set_project_trust(&project_root(project.path())?, TrustLevel::Trusted)
        .await?;
    assert!(
        list(home.path(), project.path()).await?[1]
            .inactive_reason
            .is_none()
    );
    assert_eq!(list(home.path(), other.path()).await?.len(), 1);
    assert!(
        resource_json(home.path(), other.path(), Some("project"))
            .await
            .is_err()
    );
    assert!(
        prepare(
            home.path(),
            project.path(),
            create("escape", other.path().to_str())
        )
        .await
        .is_err()
    );
    let mut invalid = create("invalid", None);
    invalid.definition.as_mut().unwrap().matcher = Some("[".into());
    assert!(prepare(home.path(), project.path(), invalid).await.is_err());
    assert!(
        serde_json::from_value::<HookChangeRequest>(
            json!({"action":"enable","id":"global","approved":true})
        )
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn elicitation_requires_explicit_human_accept_and_rechecks_revision() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let (session, mut turn, events) = crate::chaos::make_session_and_context_with_rx().await;
    let context = Arc::make_mut(&mut turn);
    Arc::make_mut(&mut context.config).chaos_home = home.path().into();
    context.approval_policy = chaos_sysctl::Constrained::allow_any(ApprovalPolicy::Interactive);
    session.permission_actor.register_turn(&turn).await?;
    *session.active_turn.lock().await = Some(crate::state::ActiveTurn::default());
    for (action, approve, stale, accepted) in [
        (ElicitationAction::Decline, true, false, false),
        (ElicitationAction::Cancel, true, false, false),
        (ElicitationAction::Accept, false, false, false),
        (ElicitationAction::Accept, true, true, false),
        (ElicitationAction::Accept, true, false, true),
    ] {
        let id = if stale { "stale" } else { "test" };
        let change = prepare(home.path(), &turn.cwd, create(id, None)).await?;
        let respond = async {
            loop {
                let event = events.recv().await?;
                if let EventMsg::ElicitationRequest(request) = event.msg {
                    let chaos_ipc::approvals::ElicitationRequest::Form { message, .. } =
                        request.request
                    else {
                        panic!("form required")
                    };
                    assert!(
                        message.contains("printf hello") && message.contains("recurring execution")
                    );
                    assert!(
                        list(home.path(), &turn.cwd)
                            .await?
                            .iter()
                            .all(|h| h.hook.id != id)
                    );
                    if stale {
                        prepare(home.path(), &turn.cwd, create(id, None))
                            .await?
                            .commit()
                            .await?;
                    }
                    session
                        .resolve_elicitation(
                            request.server_name,
                            chaos_mcp_runtime::manager::protocol_request_id_to_guest(&request.id),
                            ElicitationResponse {
                                action,
                                content: Some(json!({"approve": approve})),
                                meta: None,
                            },
                        )
                        .await?;
                    return Ok::<_, anyhow::Error>(());
                }
            }
        };
        let (result, response) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(change.authorize(&session, &turn), respond)
        })
        .await?;
        response?;
        assert_eq!(result.is_ok(), accepted);
    }
    let context = Arc::make_mut(&mut turn);
    context.vfs_policy = chaos_ipc::permissions::VfsPolicy::unrestricted();
    context.socket_policy = chaos_ipc::permissions::SocketPolicy::Enabled;
    session.permission_actor.register_turn(&turn).await?;
    let request = chaos_dtrace::BeforeTurnRequest {
        session_id: session.conversation_id,
        turn_id: turn.sub_id.clone(),
        cwd: turn.cwd.clone(),
        transcript_path: None,
        model: "test".into(),
        permission_mode: "interactive".into(),
        input_messages: Vec::new(),
        agent_context: Default::default(),
    };
    let hooks = for_turn(&session, &turn).await?;
    assert_eq!(hooks.preview_before_turn(&request).len(), 2);
    let outcome = hooks.run_before_turn(request.clone()).await;
    assert!(
        outcome
            .additional_context
            .as_deref()
            .is_some_and(|s| s.contains("hello"))
    );
    crate::user_settings::open(home.path())
        .await?
        .revoke_approvals(&crate::user_settings::installation_id(home.path())?, None)
        .await?;
    assert!(
        for_turn(&session, &turn)
            .await?
            .preview_before_turn(&request)
            .is_empty()
    );
    let context = Arc::make_mut(&mut turn);
    context.approval_policy = chaos_sysctl::Constrained::allow_any(ApprovalPolicy::Headless);
    session.permission_actor.register_turn(&turn).await?;
    assert!(
        prepare(home.path(), &turn.cwd, create("headless", None))
            .await?
            .authorize(&session, &turn)
            .await
            .is_err()
    );
    assert_eq!(list(home.path(), &turn.cwd).await?.len(), 2);
    Ok(())
}

#[tokio::test]
async fn automatic_policy_manages_hooks_without_elicitation() -> anyhow::Result<()> {
    use crate::tools::context::{ToolInvocation, ToolPayload};
    use crate::tools::handlers::HooksHandler;
    use crate::tools::registry::ToolHandler;

    let home = tempfile::tempdir()?;
    let (session, mut turn, events) = crate::chaos::make_session_and_context_with_rx().await;
    let context = Arc::make_mut(&mut turn);
    let config = Arc::make_mut(&mut context.config);
    config.chaos_home = home.path().into();
    config.hook_approval_policy = crate::config::HookApprovalPolicy::Automatic;
    context.approval_policy = chaos_sysctl::Constrained::allow_any(ApprovalPolicy::Headless);
    session.permission_actor.register_turn(&turn).await?;
    let invoke = |name: &str, arguments: serde_json::Value| {
        HooksHandler.handle(ToolInvocation {
            session: Arc::clone(&session),
            turn: Arc::clone(&turn),
            tracker: Default::default(),
            call_id: "hook-test".into(),
            tool_name: name.into(),
            tool_namespace: None,
            payload: ToolPayload::Function {
                arguments: arguments.to_string(),
            },
        })
    };
    let definition = json!({"event":"before_turn", "command":"printf hello"});
    // Preview is side-effect-free even when changes have standing authorization.
    invoke(
        "hooks_preview",
        json!({"action":"create", "id":"resident", "definition":definition}),
    )
    .await?;
    assert!(list(home.path(), &turn.cwd).await?.is_empty());
    invoke(
        "hooks_create",
        json!({"id":"resident", "definition":definition}),
    )
    .await?;
    let created = list(home.path(), &turn.cwd).await?.remove(0);
    assert!(!created.hook.enabled && !created.hook.approved);
    let revision = created.hook.revision;
    invoke(
        "hooks_set_enabled",
        json!({"id":"resident", "expected_revision":revision, "enabled":true}),
    )
    .await?;
    let enabled = list(home.path(), &turn.cwd).await?.remove(0);
    assert!(enabled.hook.enabled && enabled.hook.approved);
    assert!(
        invoke(
            "hooks_delete",
            json!({"id":"resident", "expected_revision":revision})
        )
        .await
        .is_err()
    );
    assert!(
        invoke(
            "hooks_update",
            json!({"id":"resident", "expected_revision":enabled.hook.revision,
        "definition":{"event":"before_turn", "command":""}})
        )
        .await
        .is_err()
    );
    let other = tempfile::tempdir()?;
    assert!(
        invoke(
            "hooks_update",
            json!({"id":"resident", "expected_revision":enabled.hook.revision,
        "definition":{"event":"before_turn", "command":"printf escape", "project":other.path()}})
        )
        .await
        .is_err()
    );
    invoke(
        "hooks_update",
        json!({"id":"resident", "expected_revision":enabled.hook.revision,
        "definition":{"event":"stop", "command":"printf updated"}}),
    )
    .await?;
    let updated = list(home.path(), &turn.cwd).await?.remove(0);
    assert!(updated.hook.approved && updated.hook.enabled);
    assert_eq!(updated.hook.definition.command, "printf updated");
    assert!(updated.hook.revision > enabled.hook.revision);
    // The policy does not reactivate revoked grants merely by reading hooks.
    crate::user_settings::open(home.path())
        .await?
        .revoke_approvals(&crate::user_settings::installation_id(home.path())?, None)
        .await?;
    assert_eq!(
        list(home.path(), &turn.cwd).await?[0]
            .inactive_reason
            .as_deref(),
        Some("awaiting approval on this installation")
    );
    invoke(
        "hooks_set_enabled",
        json!({"id":"resident", "expected_revision":updated.hook.revision, "enabled":false}),
    )
    .await?;
    let disabled = list(home.path(), &turn.cwd).await?.remove(0);
    assert!(!disabled.hook.enabled && !disabled.hook.approved);
    invoke(
        "hooks_delete",
        json!({"id":"resident", "expected_revision":disabled.hook.revision}),
    )
    .await?;
    assert!(list(home.path(), &turn.cwd).await?.is_empty());
    // No human endpoint or active turn was installed. No operation should solicit one.
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(event.msg, EventMsg::ElicitationRequest(_)));
    }
    Ok(())
}
