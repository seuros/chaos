//! Database-only hook management. Prepared changes are immutable while the operator reviews them.
use anyhow::{Context, ensure};
use chaos_ipc::config_types::TrustLevel;
use chaos_ipc::hooks::{HookDefinition, HookRegistration};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookAction {
    Create,
    Update,
    Enable,
    Disable,
    Delete,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookChangeRequest {
    pub action: HookAction,
    pub id: String,
    pub expected_revision: Option<i64>,
    pub definition: Option<HookDefinition>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct HookView {
    #[serde(flatten)]
    pub hook: HookRegistration,
    pub inactive_reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PreparedHookChange {
    id: String,
    expected_revision: i64,
    before: Option<HookRegistration>,
    definition: Option<HookDefinition>,
    enabled: bool,
    caller_cwd: PathBuf,
    #[serde(skip)]
    home: PathBuf,
    #[serde(skip)]
    installation: String,
}

pub fn project_root(cwd: &Path) -> anyhow::Result<PathBuf> {
    let cwd = std::fs::canonicalize(cwd).context("cannot resolve project directory")?;
    let root = crate::git_info::resolve_root_git_project_for_trust(&cwd).unwrap_or(cwd);
    Ok(std::fs::canonicalize(root)?)
}

pub fn validate_definition(definition: &HookDefinition) -> anyhow::Result<()> {
    ensure!(
        !definition.command.trim().is_empty()
            && definition.command.len() <= 16_384
            && !definition.command.contains('\0'),
        "hook command must contain 1..16384 bytes without NUL"
    );
    ensure!(
        (1..=600).contains(&definition.timeout_sec),
        "timeout_sec must be 1..600"
    );
    if let Some(matcher) = &definition.matcher {
        ensure!(
            definition.event != chaos_ipc::protocol::HookEventName::Stop,
            "Stop hooks do not support matchers"
        );
        ensure!(matcher.len() <= 4096, "matcher is too long");
        regex::Regex::new(matcher).context("invalid hook matcher")?;
    }
    Ok(())
}

pub async fn list(home: &Path, cwd: &Path) -> anyhow::Result<Vec<HookView>> {
    let root = project_root(cwd)?;
    let installation = crate::user_settings::installation_id(home)?;
    let db = crate::user_settings::open(home).await?;
    let hooks = db.list_hooks(&installation).await?;
    let trusted = db.get_project_trust(&root).await? == Some(TrustLevel::Trusted);
    let mut hooks: Vec<_> = hooks
        .into_iter()
        .filter(|h| h.definition.project.as_ref().is_none_or(|p| p == &root))
        .map(|hook| {
            let inactive_reason = if !hook.enabled {
                Some("disabled")
            } else if !hook.approved {
                Some("awaiting approval on this installation")
            } else if hook.definition.project.is_some() && !trusted {
                Some("project is not trusted")
            } else {
                None
            };
            HookView {
                hook,
                inactive_reason: inactive_reason.map(str::to_owned),
            }
        })
        .collect();
    hooks.sort_by(|a, b| {
        (
            a.hook.definition.project.is_some(),
            a.hook.definition.order,
            &a.hook.id,
        )
            .cmp(&(
                b.hook.definition.project.is_some(),
                b.hook.definition.order,
                &b.hook.id,
            ))
    });
    Ok(hooks)
}

pub async fn resource_json(home: &Path, cwd: &Path, id: Option<&str>) -> Result<String, String> {
    let hooks = list(home, cwd).await.map_err(|e| e.to_string())?;
    let value = if let Some(id) = id {
        serde_json::to_value(
            hooks
                .into_iter()
                .find(|h| h.hook.id == id)
                .ok_or_else(|| "hook not found in caller scope".to_string())?,
        )
    } else {
        serde_json::to_value(hooks)
    };
    value
        .and_then(|v| serde_json::to_string(&v))
        .map_err(|e| e.to_string())
}

pub async fn prepare(
    home: &Path,
    cwd: &Path,
    request: HookChangeRequest,
) -> anyhow::Result<PreparedHookChange> {
    ensure!(
        !request.id.is_empty()
            && request.id.len() <= 64
            && request
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)),
        "id must contain 1..64 ASCII letters, digits, '-', '_' or '.'"
    );
    let before = list(home, cwd)
        .await?
        .into_iter()
        .find(|h| h.hook.id == request.id)
        .map(|h| h.hook);
    let creating = matches!(request.action, HookAction::Create);
    let expected_revision = if creating {
        ensure!(before.is_none(), "hook already exists");
        ensure!(
            request.expected_revision.is_none_or(|r| r == 0),
            "create expects revision 0"
        );
        0
    } else {
        let before = before.as_ref().context("hook not found in caller scope")?;
        ensure!(
            request.expected_revision == Some(before.revision),
            "hook revision conflict; read chaos://hooks first"
        );
        before.revision
    };
    let (mut definition, enabled) = match request.action {
        HookAction::Create | HookAction::Update => (
            Some(request.definition.context("definition is required")?),
            request
                .enabled
                .unwrap_or_else(|| before.as_ref().is_some_and(|h| h.enabled)),
        ),
        action => {
            ensure!(
                request.definition.is_none() && request.enabled.is_none(),
                "this action does not accept definition or enabled"
            );
            let definition = before.as_ref().map(|h| h.definition.clone());
            match action {
                HookAction::Enable => (definition, true),
                HookAction::Disable => (definition, false),
                HookAction::Delete => (None, false),
                _ => unreachable!(),
            }
        }
    };
    if let Some(definition) = &mut definition {
        if let Some(project) = &definition.project {
            let project = project_root(&cwd.join(project))?;
            ensure!(
                project == project_root(cwd)?,
                "hooks can only target the current project"
            );
            definition.project = Some(project);
        }
        validate_definition(definition)?;
    }
    Ok(PreparedHookChange {
        id: request.id,
        expected_revision,
        before,
        definition,
        enabled,
        caller_cwd: std::fs::canonicalize(cwd)?,
        home: home.into(),
        installation: crate::user_settings::installation_id(home)?,
    })
}

impl PreparedHookChange {
    pub fn approval_message(&self) -> anyhow::Result<String> {
        Ok(format!(
            "Approve this exact hook change?\n{}\n{}\nHooks run shell commands repeatedly at their selected lifecycle event, under the session sandbox. No hook is executed by this management operation.",
            serde_json::to_string_pretty(self)?,
            if self.enabled {
                "This authorizes recurring execution on THIS installation."
            } else {
                "This does NOT authorize execution; the hook will be disabled or deleted."
            }
        ))
    }

    // Not exposed as a tool; the model cannot supply approval data.
    async fn commit(self) -> anyhow::Result<i64> {
        crate::user_settings::open(&self.home)
            .await?
            .commit_hook(
                &self.id,
                self.expected_revision,
                self.definition.as_ref(),
                self.enabled,
                &self.installation,
            )
            .await
    }

    pub async fn confirm_terminal(self, yes: bool) -> anyhow::Result<i64> {
        confirm_terminal(&self.approval_message()?, yes)?;
        self.commit().await
    }

    pub(crate) async fn authorize(
        self,
        session: &crate::chaos::Session,
        turn: &crate::chaos::TurnContext,
    ) -> anyhow::Result<i64> {
        use chaos_ipc::api::{McpServerElicitationRequest, McpServerElicitationRequestParams};
        use chaos_ipc::protocol::ApprovalPolicy;
        use chaos_mcp_runtime::{ElicitationAction, McpRequestId};
        if turn.config.hook_approval_policy == crate::config::HookApprovalPolicy::Automatic {
            return self.commit().await;
        }
        let policy = session.permission_snapshot(turn).await.approval_policy;
        ensure!(
            match policy {
                ApprovalPolicy::Headless => false,
                ApprovalPolicy::Granular(g) => g.allows_mcp_elicitations(),
                _ => true,
            },
            "human elicitation is unavailable; hook change denied"
        );
        let params = McpServerElicitationRequestParams {
            process_id: session.conversation_id.to_string(),
            turn_id: Some(turn.sub_id.clone()),
            server_name: "chaos-hooks".into(),
            request: McpServerElicitationRequest::Form {
                meta: None,
                message: self.approval_message()?,
                requested_schema: serde_json::from_value(serde_json::json!({
                    "type": "object", "properties": {
                        "approve": {"type": "boolean", "title": "Approve this exact change", "default": false}
                    }, "required": ["approve"]
                }))?,
            },
        };
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(300),
            session.request_mcp_server_elicitation(
                turn,
                McpRequestId::String(uuid::Uuid::new_v4().to_string()),
                params,
            ),
        )
        .await
        .context("hook approval timed out; no change made")?
        .context("hook approval cancelled; no change made")?;
        ensure!(
            response.action == ElicitationAction::Accept
                && response
                    .content
                    .as_ref()
                    .and_then(|v| v.get("approve"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true),
            "hook change declined; no change made"
        );
        self.commit().await
    }
}

fn confirm_terminal(message: &str, yes: bool) -> anyhow::Result<()> {
    use std::io::{BufRead, IsTerminal, Write};
    ensure!(
        std::env::var_os(crate::exec_env::CHAOS_THREAD_ID_ENV_VAR).is_none(),
        "agent shell commands must use hooks_* tools and the configured hook approval policy"
    );
    if yes {
        return Ok(());
    }
    ensure!(
        std::io::stdin().is_terminal(),
        "hook changes require an interactive terminal, explicit --yes, or authorized hooks_* tools"
    );
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{message}")?;
    write!(stdout, "Type yes to approve: ")?;
    stdout.flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    ensure!(answer.trim() == "yes", "hook change cancelled");
    Ok(())
}

pub async fn import_terminal(
    home: &Path,
    cwd: &Path,
    file: &Path,
    prefix: &str,
    project: bool,
    yes: bool,
) -> anyhow::Result<()> {
    ensure!(
        !prefix.is_empty()
            && prefix.len() <= 50
            && prefix
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)),
        "invalid import prefix"
    );
    let content = std::fs::read_to_string(file)?;
    let mut hooks = Vec::new();
    for (index, mut definition) in chaos_dtrace::parse_legacy_hooks(&content)?
        .into_iter()
        .enumerate()
    {
        definition.project = if project {
            Some(project_root(cwd)?)
        } else {
            None
        };
        validate_definition(&definition)?;
        hooks.push((format!("{prefix}-{}", index + 1), definition));
    }
    confirm_terminal(
        &format!(
            "Import these hooks DISABLED? No files are deleted and nothing executes.\n{}",
            serde_json::to_string_pretty(&hooks)?
        ),
        yes,
    )?;
    crate::user_settings::open(home)
        .await?
        .import_hooks(&hooks)
        .await
}

pub(crate) async fn for_turn(
    session: &crate::chaos::Session,
    turn: &crate::chaos::TurnContext,
) -> anyhow::Result<chaos_dtrace::Hooks> {
    use crate::exec::{ExecExpiration, ExecParams, ExecSandboxContext, build_exec_request};
    use crate::sandboxing::SandboxPermissions;
    use std::sync::Arc;
    let registrations: Vec<_> = list(&turn.config.chaos_home, &turn.cwd)
        .await?
        .into_iter()
        .filter(|h| h.inactive_reason.is_none())
        .map(|h| h.hook)
        .collect();
    for hook in &registrations {
        validate_definition(&hook.definition)?;
    }
    if registrations.is_empty() {
        return Ok(chaos_dtrace::Hooks::default());
    }
    let shell = session.user_shell();
    let mut argv = shell.derive_exec_args("", false);
    let program = argv.remove(0);
    argv.pop();
    let permissions = session.permission_snapshot(turn).await;
    let vfs = permissions.effective_vfs_policy();
    let socket = permissions.effective_socket_policy();
    for hook in &registrations {
        let command = shell.derive_exec_args(&hook.definition.command, false);
        let requirement = session
            .services
            .exec_policy
            .create_exec_approval_requirement_for_command_in(
                crate::exec_policy::ExecApprovalRequest {
                    command: &command,
                    // Hook activation is the recurring execution approval; never overrides a deny rule.
                    approval_policy: chaos_ipc::protocol::ApprovalPolicy::Interactive,
                    vfs_policy: &vfs,
                    sandbox_permissions: SandboxPermissions::UseDefault,
                    prefix_rule: None,
                },
                &turn.cwd,
            )
            .await;
        if let crate::tools::sandboxing::ExecApprovalRequirement::Forbidden { reason } = requirement
        {
            anyhow::bail!(
                "hook {} is forbidden by execution policy: {reason}",
                hook.id
            );
        }
    }
    let alcatraz = turn.alcatraz_exe.clone();
    let sandbox_cwd = turn.cwd.clone();
    let network = turn.network.clone();
    let env = crate::exec_env::create_env(
        &turn.shell_environment_policy,
        Some(session.conversation_id),
    );
    let builder = Arc::new(
        move |command: Vec<String>, cwd: &Path| -> anyhow::Result<tokio::process::Command> {
            let request = build_exec_request(
                ExecParams {
                    command,
                    cwd: cwd.into(),
                    expiration: ExecExpiration::Timeout(std::time::Duration::from_secs(600)),
                    env: env.clone(),
                    network: network.clone(),
                    sandbox_permissions: SandboxPermissions::UseDefault,
                    justification: None,
                    arg0: None,
                },
                ExecSandboxContext {
                    vfs_policy: &vfs,
                    socket_policy: socket,
                    sandbox_cwd: &sandbox_cwd,
                    alcatraz_exe: &alcatraz,
                },
            )?;
            let (program, args) = request
                .command
                .split_first()
                .context("empty hook command")?;
            let mut command = tokio::process::Command::new(program);
            #[cfg(unix)]
            if let Some(arg0) = request.arg0 {
                command.arg0(arg0);
            }
            command.args(args).env_clear().envs(request.env);
            Ok(command)
        },
    );
    Ok(chaos_dtrace::Hooks::new(chaos_dtrace::HooksConfig {
        registrations,
        shell_program: Some(program),
        shell_args: argv,
        command_builder: Some(builder),
    }))
}

#[cfg(test)]
mod tests;
