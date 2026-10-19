use super::*;

#[test]
fn granular_policy_without_permission_requests_is_auto_denied() {
    let policy = ApprovalPolicy::Granular(crate::protocol::GranularApprovalConfig {
        sandbox_approval: true,
        rules: true,
        request_permissions: false,
        mcp_elicitations: true,
    });

    assert_eq!(
        automatic_request_permissions_response(policy),
        Some(RequestPermissionsResponse {
            permissions: RequestPermissionProfile::default(),
            scope: PermissionGrantScope::Turn,
        })
    );
}

#[test]
fn interactive_policy_requires_a_client_response() {
    assert_eq!(
        automatic_request_permissions_response(ApprovalPolicy::Interactive),
        None
    );
}

#[tokio::test]
async fn dropped_input_releases_mailbox_slot() {
    let state = std::sync::Arc::new(tokio::sync::Mutex::new(
        crate::state::turn::TurnState::default(),
    ));
    let (sender, receiver) = oneshot::channel();
    assert!(matches!(
        state
            .lock()
            .await
            .insert_pending_user_input("turn".into(), sender),
        PendingInsert::Inserted
    ));
    drop(PendingUserInput {
        receiver,
        state: state.clone(),
        key: "turn".into(),
    });
    assert!(
        state
            .lock()
            .await
            .remove_pending_user_input("turn")
            .is_none()
    );
}

#[tokio::test]
async fn delayed_input_cleanup_preserves_live_replacement() {
    let state = std::sync::Arc::new(tokio::sync::Mutex::new(
        crate::state::turn::TurnState::default(),
    ));
    let (sender, receiver) = oneshot::channel();
    let mut locked = state.lock().await;
    assert!(matches!(
        locked.insert_pending_user_input("turn".into(), sender),
        PendingInsert::Inserted
    ));
    // Force the guard's delayed cleanup path.
    drop(PendingUserInput {
        receiver,
        state: state.clone(),
        key: "turn".into(),
    });
    let (replacement, mut response) = oneshot::channel();
    assert!(matches!(
        locked.insert_pending_user_input("turn".into(), replacement),
        PendingInsert::Inserted
    ));
    drop(locked);
    tokio::task::yield_now().await;
    let sender = state.lock().await.remove_pending_user_input("turn");
    assert!(sender.is_some());
    sender
        .unwrap()
        .send(RequestUserInputResponse {
            answers: Default::default(),
        })
        .unwrap();
    assert!(response.try_recv().is_ok());
}

#[test]
fn input_cleanup_can_drop_outside_runtime_with_contended_state() {
    let state = std::sync::Arc::new(tokio::sync::Mutex::new(
        crate::state::turn::TurnState::default(),
    ));
    let (sender, receiver) = oneshot::channel();
    let mut locked = state.try_lock().unwrap();
    assert!(matches!(
        locked.insert_pending_user_input("turn".into(), sender),
        PendingInsert::Inserted
    ));
    drop(PendingUserInput {
        receiver,
        state: state.clone(),
        key: "turn".into(),
    });
    let (replacement, _response) = oneshot::channel();
    assert!(matches!(
        locked.insert_pending_user_input("turn".into(), replacement),
        PendingInsert::Inserted
    ));
}
