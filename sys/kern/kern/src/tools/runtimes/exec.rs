/*
Runtime: exec

Handles approval + sandbox orchestration for managed exec requests, delegating to
the process manager to spawn PTYs once an ExecRequest is prepared.
*/
use crate::command_canonicalization::canonicalize_command_for_approval;
use crate::error::ChaosErr;
use crate::error::SandboxErr;
use crate::exec::ExecError;
use crate::exec::ExecExpiration;
use crate::exec::ExecProcess;
use crate::exec::ExecProcessManager;
use crate::exec::NoopSpawnLifecycle;
use crate::sandboxing::SandboxPermissions;
use crate::tools::network_approval::NetworkApprovalMode;
use crate::tools::network_approval::NetworkApprovalSpec;
use crate::tools::runtimes::build_command_spec;
use crate::tools::runtimes::maybe_apply_shell_environment;
use crate::tools::sandboxing::Approvable;
use crate::tools::sandboxing::ApprovalCtx;
use crate::tools::sandboxing::ExecApprovalRequirement;
use crate::tools::sandboxing::SandboxAttempt;
use crate::tools::sandboxing::SandboxOverride;
use crate::tools::sandboxing::Sandboxable;
use crate::tools::sandboxing::SandboxablePreference;
use crate::tools::sandboxing::ToolCtx;
use crate::tools::sandboxing::ToolError;
use crate::tools::sandboxing::ToolRuntime;
use crate::tools::sandboxing::sandbox_override_for_first_attempt;
use crate::tools::sandboxing::with_cached_approval;
use chaos_ipc::models::PermissionProfile;
use chaos_ipc::protocol::ReviewDecision;
use chaos_pf::NetworkProxy;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct ExecRequest {
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub explicit_env_overrides: HashMap<String, String>,
    pub network: Option<NetworkProxy>,
    pub tty: bool,
    pub sandbox_permissions: SandboxPermissions,
    pub additional_permissions: Option<PermissionProfile>,
    pub justification: Option<String>,
    pub exec_approval_requirement: ExecApprovalRequirement,
}

#[derive(serde::Serialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ExecApprovalKey {
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub tty: bool,
    pub sandbox_permissions: SandboxPermissions,
    pub additional_permissions: Option<PermissionProfile>,
}

pub struct ExecRuntime<'a> {
    manager: &'a ExecProcessManager,
}

impl<'a> ExecRuntime<'a> {
    pub fn new(manager: &'a ExecProcessManager) -> Self {
        Self { manager }
    }
}

impl Sandboxable for ExecRuntime<'_> {
    fn sandbox_preference(&self) -> SandboxablePreference {
        SandboxablePreference::Auto
    }

    fn escalate_on_failure(&self) -> bool {
        true
    }
}

impl Approvable<ExecRequest> for ExecRuntime<'_> {
    type ApprovalKey = ExecApprovalKey;

    fn approval_keys(&self, req: &ExecRequest) -> Vec<Self::ApprovalKey> {
        vec![ExecApprovalKey {
            command: canonicalize_command_for_approval(&req.command),
            cwd: req.cwd.clone(),
            tty: req.tty,
            sandbox_permissions: req.sandbox_permissions,
            additional_permissions: req.additional_permissions.clone(),
        }]
    }

    fn start_approval_async<'b>(
        &'b mut self,
        req: &'b ExecRequest,
        ctx: ApprovalCtx<'b>,
    ) -> BoxFuture<'b, ReviewDecision> {
        let keys = self.approval_keys(req);
        let session = ctx.session;
        let turn = ctx.turn;
        let call_id = ctx.call_id.to_string();
        let command = req.command.clone();
        let cwd = req.cwd.clone();
        let retry_reason = ctx.retry_reason.clone();
        let reason = retry_reason.or_else(|| req.justification.clone());
        Box::pin(async move {
            with_cached_approval(&session.services, "exec", keys, || async move {
                let available_decisions = None;
                session
                    .request_command_approval(
                        turn,
                        call_id,
                        /*approval_id*/ None,
                        command,
                        cwd,
                        reason,
                        ctx.network_approval_context.clone(),
                        req.exec_approval_requirement
                            .proposed_execpolicy_amendment()
                            .cloned(),
                        req.additional_permissions.clone(),
                        available_decisions,
                    )
                    .await
            })
            .await
        })
    }

    fn exec_approval_requirement(&self, req: &ExecRequest) -> Option<ExecApprovalRequirement> {
        Some(req.exec_approval_requirement.clone())
    }

    fn sandbox_mode_for_first_attempt(&self, req: &ExecRequest) -> SandboxOverride {
        sandbox_override_for_first_attempt(req.sandbox_permissions, &req.exec_approval_requirement)
    }
}

impl<'a> ToolRuntime<ExecRequest, ExecProcess> for ExecRuntime<'a> {
    fn allows_no_sandbox_retry(&self, req: &ExecRequest) -> bool {
        !req.sandbox_permissions.uses_additional_permissions()
    }

    fn network_approval_spec(
        &self,
        req: &ExecRequest,
        _ctx: &ToolCtx,
    ) -> Option<NetworkApprovalSpec> {
        req.network.as_ref()?;
        Some(NetworkApprovalSpec {
            network: req.network.clone(),
            mode: NetworkApprovalMode::Deferred,
        })
    }

    async fn run(
        &mut self,
        req: &ExecRequest,
        attempt: &SandboxAttempt<'_>,
        ctx: &ToolCtx,
    ) -> Result<ExecProcess, ToolError> {
        let base_command = &req.command;
        let session_shell = ctx.session.user_shell();
        let (command, mut env) = maybe_apply_shell_environment(
            base_command,
            session_shell.as_ref(),
            &req.cwd,
            &ctx.turn.shell_environment_policy,
            ctx.session.conversation_id,
            &req.env,
            &req.explicit_env_overrides,
        );

        if let Some(network) = req.network.as_ref() {
            network.apply_to_env(&mut env);
        }
        let spec = build_command_spec(
            &command,
            &req.cwd,
            &env,
            ExecExpiration::DefaultTimeout,
            req.sandbox_permissions,
            req.additional_permissions.clone(),
            req.justification.clone(),
        )
        .map_err(|_| ToolError::Rejected("missing command line for PTY".to_string()))?;
        let exec_env = attempt
            .env_for(spec, req.network.as_ref())
            .map_err(|err| ToolError::Chaos(err.into()))?;
        self.manager
            .open_session_with_exec_env(&exec_env, req.tty, Box::new(NoopSpawnLifecycle))
            .await
            .map_err(|err| match err {
                ExecError::SandboxDenied { output, .. } => {
                    ToolError::Chaos(ChaosErr::Sandbox(SandboxErr::Denied {
                        output: Box::new(output),
                        network_policy_decision: None,
                    }))
                }
                other => ToolError::Rejected(other.to_string()),
            })
    }
}
