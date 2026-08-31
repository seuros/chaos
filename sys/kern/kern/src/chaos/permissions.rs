//! Session and turn permission actor.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use chaos_ipc::models::PermissionProfile;
use chaos_ipc::permissions::SocketPolicy;
use chaos_ipc::permissions::VfsPolicy;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::PermissionGrantUpdate;
use chaos_ipc::protocol::PermissionUpdateScope;
use chaos_ipc::protocol::PermissionsUpdatedEvent;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_parole::sandbox::vfs_policy_from_sandbox_policy;
use chaos_traits::router::Adapter;
use chaos_traits::router::AdapterError;
use chaos_traits::router::DEFAULT_ADAPTER_CAPACITY;
use tokio::sync::oneshot;

use crate::sandboxing::effective_socket_policy;
use crate::sandboxing::effective_vfs_policy;
use crate::sandboxing::merge_permission_profiles;

use super::SessionConfiguration;
use super::TurnContext;

#[derive(Debug, Clone)]
pub(crate) struct PermissionSnapshot {
    pub(crate) revision: u64,
    pub(crate) approval_policy: ApprovalPolicy,
    pub(crate) vfs_policy: VfsPolicy,
    pub(crate) socket_policy: SocketPolicy,
    pub(crate) granted_permissions: Option<PermissionProfile>,
}

impl PermissionSnapshot {
    pub(crate) fn effective_vfs_policy(&self) -> VfsPolicy {
        effective_vfs_policy(&self.vfs_policy, self.granted_permissions.as_ref())
    }

    pub(crate) fn effective_socket_policy(&self) -> SocketPolicy {
        effective_socket_policy(self.socket_policy, self.granted_permissions.as_ref())
    }
}

#[derive(Debug, Clone)]
struct PermissionLayer {
    revision: u64,
    approval_policy: ApprovalPolicy,
    vfs_policy: VfsPolicy,
    socket_policy: SocketPolicy,
    granted_permissions: Option<PermissionProfile>,
    cwd: PathBuf,
    approval_overridden: bool,
    sandbox_overridden: bool,
}

impl PermissionLayer {
    fn new(
        approval_policy: ApprovalPolicy,
        vfs_policy: VfsPolicy,
        socket_policy: SocketPolicy,
        cwd: PathBuf,
    ) -> Self {
        Self {
            revision: 0,
            approval_policy,
            vfs_policy,
            socket_policy,
            granted_permissions: None,
            cwd,
            approval_overridden: false,
            sandbox_overridden: false,
        }
    }

    fn from_configuration(configuration: &SessionConfiguration) -> Self {
        Self::new(
            configuration.approval_policy.value(),
            configuration.vfs_policy.clone(),
            configuration.socket_policy,
            configuration.cwd.clone(),
        )
    }

    fn from_turn(turn: &TurnContext) -> Self {
        Self::new(
            turn.approval_policy.value(),
            turn.vfs_policy.clone(),
            turn.socket_policy,
            turn.cwd.clone(),
        )
    }

    fn mark_initial_turn_overrides(&mut self, session: &Self) {
        self.approval_overridden = self.approval_policy != session.approval_policy;
        self.sandbox_overridden =
            self.vfs_policy != session.vfs_policy || self.socket_policy != session.socket_policy;
    }

    fn sync_session_defaults(&mut self, mut next: Self) {
        next.revision = self.revision;
        next.granted_permissions = self.granted_permissions.take();
        *self = next;
    }

    fn snapshot(&self) -> PermissionSnapshot {
        PermissionSnapshot {
            revision: self.revision,
            approval_policy: self.approval_policy,
            vfs_policy: self.vfs_policy.clone(),
            socket_policy: self.socket_policy,
            granted_permissions: self.granted_permissions.clone(),
        }
    }

    fn apply_without_revision_check(
        &mut self,
        approval_policy: Option<ApprovalPolicy>,
        sandbox_policy: Option<SandboxPolicy>,
        grants: PermissionGrantUpdate,
        cwd: &Path,
        mark_turn_overrides: bool,
    ) {
        if let Some(approval_policy) = approval_policy {
            self.approval_policy = approval_policy;
            if mark_turn_overrides {
                self.approval_overridden = true;
            }
        }
        if let Some(sandbox_policy) = sandbox_policy {
            self.vfs_policy = vfs_policy_from_sandbox_policy(&sandbox_policy, cwd);
            self.socket_policy = SocketPolicy::from(&sandbox_policy);
            if mark_turn_overrides {
                self.sandbox_overridden = true;
            }
        }
        match grants {
            PermissionGrantUpdate::Unchanged => {}
            PermissionGrantUpdate::Merge(permissions) => {
                self.granted_permissions = merge_permission_profiles(
                    self.granted_permissions.as_ref(),
                    Some(&permissions),
                );
            }
            PermissionGrantUpdate::Replace(permissions) => {
                self.granted_permissions = (!permissions.is_empty()).then_some(permissions);
            }
            PermissionGrantUpdate::Clear => {
                self.granted_permissions = None;
            }
        }
        self.bump_revision();
    }

    fn apply(
        &mut self,
        expected_revision: Option<u64>,
        approval_policy: Option<ApprovalPolicy>,
        sandbox_policy: Option<SandboxPolicy>,
        grants: PermissionGrantUpdate,
        cwd: &Path,
        mark_turn_overrides: bool,
    ) -> Result<PermissionSnapshot, PermissionUpdateError> {
        if let Some(expected) = expected_revision
            && expected != self.revision
        {
            return Err(PermissionUpdateError::RevisionConflict {
                expected,
                actual: self.revision,
            });
        }

        self.apply_without_revision_check(
            approval_policy,
            sandbox_policy,
            grants,
            cwd,
            mark_turn_overrides,
        );
        Ok(self.snapshot())
    }

    fn inherit_session_update(
        &mut self,
        approval_policy: Option<ApprovalPolicy>,
        sandbox_policy: Option<&SandboxPolicy>,
        grants_changed: bool,
    ) {
        let mut effective_changed = grants_changed;
        if let Some(approval_policy) = approval_policy
            && !self.approval_overridden
            && self.approval_policy != approval_policy
        {
            self.approval_policy = approval_policy;
            effective_changed = true;
        }
        if let Some(sandbox_policy) = sandbox_policy
            && !self.sandbox_overridden
        {
            let vfs_policy = vfs_policy_from_sandbox_policy(sandbox_policy, &self.cwd);
            let socket_policy = SocketPolicy::from(sandbox_policy);
            if self.vfs_policy != vfs_policy || self.socket_policy != socket_policy {
                self.vfs_policy = vfs_policy;
                self.socket_policy = socket_policy;
                effective_changed = true;
            }
        }
        if effective_changed {
            self.bump_revision();
        }
    }

    fn bump_revision(&mut self) {
        self.revision = self
            .revision
            .checked_add(1)
            .unwrap_or_else(|| panic!("permission revision overflow"));
    }
}

fn effective_turn_snapshot(
    session: &PermissionLayer,
    turn: &PermissionLayer,
) -> PermissionSnapshot {
    let mut snapshot = turn.snapshot();
    snapshot.granted_permissions = merge_permission_profiles(
        session.granted_permissions.as_ref(),
        turn.granted_permissions.as_ref(),
    );
    snapshot
}

fn permission_updated(
    scope: PermissionUpdateScope,
    snapshot: PermissionSnapshot,
) -> PermissionsUpdatedEvent {
    PermissionsUpdatedEvent {
        scope,
        revision: snapshot.revision,
        approval_policy: snapshot.approval_policy,
        vfs_policy: snapshot.vfs_policy,
        socket_policy: snapshot.socket_policy,
        granted_permissions: snapshot.granted_permissions,
    }
}

async fn receive<T>(response: oneshot::Receiver<T>) -> Result<T, PermissionUpdateError> {
    response
        .await
        .map_err(|_| PermissionUpdateError::Actor(AdapterError::ReplyDropped))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PermissionUpdateError {
    #[error("permission actor is unavailable: {0}")]
    Actor(#[from] AdapterError),
    #[error("permission revision conflict: expected {expected}, current revision is {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("active turn `{0}` is not registered")]
    UnknownTurn(String),
}

#[derive(Debug)]
enum PermissionCommand {
    SetSessionDefaults {
        layer: PermissionLayer,
        reply: oneshot::Sender<()>,
    },
    RegisterTurn {
        turn_id: String,
        layer: PermissionLayer,
        reply: oneshot::Sender<()>,
    },
    RemoveTurn {
        turn_id: String,
        reply: oneshot::Sender<()>,
    },
    Snapshot {
        turn_id: String,
        reply: oneshot::Sender<Result<PermissionSnapshot, PermissionUpdateError>>,
    },
    Update {
        scope: PermissionUpdateScope,
        expected_revision: Option<u64>,
        approval_policy: Option<ApprovalPolicy>,
        sandbox_policy: Option<SandboxPolicy>,
        grants: PermissionGrantUpdate,
        cwd: std::path::PathBuf,
        reply: oneshot::Sender<Result<PermissionsUpdatedEvent, PermissionUpdateError>>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct PermissionActor {
    mailbox: Adapter<PermissionCommand>,
}

impl PermissionActor {
    pub(crate) fn spawn(configuration: &SessionConfiguration) -> Self {
        let (mailbox, mut receiver) = Adapter::bounded(DEFAULT_ADAPTER_CAPACITY);
        let mut session = PermissionLayer::from_configuration(configuration);
        let mut turns: HashMap<String, PermissionLayer> = HashMap::new();

        tokio::spawn(async move {
            while let Some(packet) = receiver.recv().await {
                match packet.op {
                    PermissionCommand::SetSessionDefaults { layer, reply } => {
                        session.sync_session_defaults(layer);
                        let _ = reply.send(());
                    }
                    PermissionCommand::RegisterTurn {
                        turn_id,
                        layer,
                        reply,
                    } => {
                        let mut layer = layer;
                        layer.mark_initial_turn_overrides(&session);
                        turns.insert(turn_id, layer);
                        let _ = reply.send(());
                    }
                    PermissionCommand::RemoveTurn { turn_id, reply } => {
                        turns.remove(&turn_id);
                        let _ = reply.send(());
                    }
                    PermissionCommand::Snapshot { turn_id, reply } => {
                        let result = turns
                            .get(&turn_id)
                            .map(|turn| effective_turn_snapshot(&session, turn))
                            .ok_or(PermissionUpdateError::UnknownTurn(turn_id));
                        let _ = reply.send(result);
                    }
                    PermissionCommand::Update {
                        scope,
                        expected_revision,
                        approval_policy,
                        sandbox_policy,
                        grants,
                        cwd,
                        reply,
                    } => {
                        let result = match &scope {
                            PermissionUpdateScope::Session => {
                                let grants_changed =
                                    !matches!(&grants, PermissionGrantUpdate::Unchanged);
                                let result = session.apply(
                                    expected_revision,
                                    approval_policy,
                                    sandbox_policy.clone(),
                                    grants,
                                    &cwd,
                                    false,
                                );
                                if result.is_ok() {
                                    for turn in turns.values_mut() {
                                        turn.inherit_session_update(
                                            approval_policy,
                                            sandbox_policy.as_ref(),
                                            grants_changed,
                                        );
                                    }
                                }
                                result
                            }
                            PermissionUpdateScope::ActiveTurn { turn_id } => turns
                                .get_mut(turn_id)
                                .ok_or_else(|| PermissionUpdateError::UnknownTurn(turn_id.clone()))
                                .and_then(|layer| {
                                    layer
                                        .apply(
                                            expected_revision,
                                            approval_policy,
                                            sandbox_policy,
                                            grants,
                                            &cwd,
                                            true,
                                        )
                                        .map(|_| effective_turn_snapshot(&session, layer))
                                }),
                        }
                        .map(|snapshot| permission_updated(scope, snapshot));
                        let _ = reply.send(result);
                    }
                }
            }
        });

        Self { mailbox }
    }

    pub(crate) async fn set_session_defaults(
        &self,
        configuration: &SessionConfiguration,
    ) -> Result<(), PermissionUpdateError> {
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(PermissionCommand::SetSessionDefaults {
                layer: PermissionLayer::from_configuration(configuration),
                reply,
            })
            .await?;
        receive(response).await
    }

    pub(crate) async fn register_turn(
        &self,
        turn: &TurnContext,
    ) -> Result<(), PermissionUpdateError> {
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(PermissionCommand::RegisterTurn {
                turn_id: turn.sub_id.clone(),
                layer: PermissionLayer::from_turn(turn),
                reply,
            })
            .await?;
        receive(response).await
    }

    pub(crate) async fn remove_turn(&self, turn_id: String) -> Result<(), PermissionUpdateError> {
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(PermissionCommand::RemoveTurn { turn_id, reply })
            .await?;
        receive(response).await
    }

    pub(crate) async fn snapshot(
        &self,
        turn_id: String,
    ) -> Result<PermissionSnapshot, PermissionUpdateError> {
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(PermissionCommand::Snapshot { turn_id, reply })
            .await?;
        receive(response).await?
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn update(
        &self,
        scope: PermissionUpdateScope,
        expected_revision: Option<u64>,
        approval_policy: Option<ApprovalPolicy>,
        sandbox_policy: Option<SandboxPolicy>,
        grants: PermissionGrantUpdate,
        cwd: std::path::PathBuf,
    ) -> Result<PermissionsUpdatedEvent, PermissionUpdateError> {
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(PermissionCommand::Update {
                scope,
                expected_revision,
                approval_policy,
                sandbox_policy,
                grants,
                cwd,
                reply,
            })
            .await?;
        receive(response).await?
    }

    pub(crate) async fn merge_grant(
        &self,
        scope: PermissionUpdateScope,
        permissions: PermissionProfile,
    ) -> Result<PermissionsUpdatedEvent, PermissionUpdateError> {
        self.update(
            scope,
            None,
            None,
            None,
            PermissionGrantUpdate::Merge(permissions),
            PathBuf::from("/"),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
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
}
