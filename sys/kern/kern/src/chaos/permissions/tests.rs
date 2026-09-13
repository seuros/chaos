use super::*;
use chaos_ipc::models::NetworkPermissions;

fn layer() -> PermissionLayer {
    PermissionLayer {
        revision: 0,
        approval_policy: ApprovalPolicy::Interactive,
        vfs_policy: VfsPolicy::default(),
        socket_policy: SocketPolicy::default(),
        granted_permissions: None,
        cwd: PathBuf::from("/"),
        approval_overridden: false,
        sandbox_overridden: false,
    }
}

#[test]
fn revision_conflicts_do_not_mutate_state() {
    let mut layer = layer();
    let err = layer
        .apply(
            Some(4),
            Some(ApprovalPolicy::Headless),
            None,
            PermissionGrantUpdate::Unchanged,
            Path::new("/"),
            false,
        )
        .expect_err("revision conflict");
    assert!(matches!(
        err,
        PermissionUpdateError::RevisionConflict {
            expected: 4,
            actual: 0
        }
    ));
    assert_eq!(layer.approval_policy, ApprovalPolicy::Interactive);
    assert_eq!(layer.revision, 0);
}

#[test]
fn grant_updates_are_revisioned() {
    let mut layer = layer();
    let snapshot = layer
        .apply(
            Some(0),
            None,
            None,
            PermissionGrantUpdate::Merge(PermissionProfile {
                network: Some(NetworkPermissions {
                    enabled: Some(true),
                }),
                ..Default::default()
            }),
            Path::new("/"),
            false,
        )
        .expect("merge grant");
    assert_eq!(snapshot.revision, 1);
    assert_eq!(snapshot.effective_socket_policy(), SocketPolicy::Enabled);
}

#[test]
fn turn_override_survives_later_session_update() {
    let mut turn = layer();
    turn.apply(
        Some(0),
        Some(ApprovalPolicy::Headless),
        None,
        PermissionGrantUpdate::Unchanged,
        Path::new("/"),
        true,
    )
    .expect("turn update");

    turn.inherit_session_update(Some(ApprovalPolicy::Supervised), None, false);

    assert_eq!(turn.approval_policy, ApprovalPolicy::Headless);
    assert_eq!(turn.revision, 1);
}

#[test]
fn syncing_session_defaults_preserves_revision_and_grants() {
    let mut session = layer();
    session.revision = 7;
    session.granted_permissions = Some(PermissionProfile {
        network: Some(NetworkPermissions {
            enabled: Some(true),
        }),
        ..Default::default()
    });
    let mut next = layer();
    next.approval_policy = ApprovalPolicy::Headless;
    next.cwd = PathBuf::from("/next");

    session.sync_session_defaults(next);

    assert_eq!(session.revision, 7);
    assert_eq!(session.approval_policy, ApprovalPolicy::Headless);
    assert_eq!(session.cwd, PathBuf::from("/next"));
    assert!(session.granted_permissions.is_some());
}
