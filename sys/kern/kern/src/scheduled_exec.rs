//! Durable shell jobs use the same execpolicy and sandbox runner as shell tools.
//! A saved policy is a constraint, not permission to bypass current policy.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::bail;
use chaos_cron::CronJob;
use chaos_cron::CronScope;
use chaos_ipc::permissions::SocketPolicy;
use chaos_ipc::permissions::VfsPolicy;
use chaos_ipc::permissions::VfsPolicyKind;
use chaos_ipc::protocol::ApprovalPolicy;
use serde::Deserialize;
use serde::Serialize;

use crate::chaos::Session;
use crate::chaos::TurnContext;
use crate::config::Config;
use crate::config::ConfigBuilder;
use crate::exec::ExecExpiration;
use crate::exec::ExecParams;
use crate::exec::process_exec_tool_call;
use crate::exec_env::create_env;
use crate::exec_policy::ExecApprovalRequest;
use crate::exec_policy::ExecPolicyManager;
use crate::exec_policy::load_exec_policy;
use crate::sandboxing::SandboxPermissions;
use crate::tools::sandboxing::ExecApprovalRequirement;

const POLICY_VERSION: u32 = 1;
const POLICY_TIMEOUT: Duration = Duration::from_secs(10);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduledShellPolicy {
    version: u32,
    command: String,
    creator_session_id: String,
    chaos_home: PathBuf,
    cwd: PathBuf,
    vfs_policy: VfsPolicy,
    socket_policy: SocketPolicy,
    approval_policy: ApprovalPolicy,
}

impl ScheduledShellPolicy {
    fn validate_config(&self, config: &Config) -> anyhow::Result<()> {
        if self.version != POLICY_VERSION
            || self.chaos_home != config.chaos_home
            || !self.cwd.is_absolute()
            || self.creator_session_id.is_empty()
        {
            bail!("invalid or foreign scheduled execution policy");
        }
        // An enclosing sandbox or a session's managed proxy cannot be assumed
        // to exist in the process that later picks up this job.
        if self.vfs_policy.kind == VfsPolicyKind::ExternalSandbox
            || config.permissions.vfs_policy.kind == VfsPolicyKind::ExternalSandbox
            || config.permissions.network.is_some()
        {
            bail!("shell cron does not support external sandboxes or managed network proxies");
        }
        if self.vfs_policy.semantic_signature(&self.cwd)
            != config
                .permissions
                .vfs_policy
                .semantic_signature(&config.cwd)
            || self.socket_policy != config.permissions.socket_policy
        {
            bail!("scheduled permissions differ from current constraints; recreate the job");
        }
        Ok(())
    }
}

fn shell_command(command: &str) -> Vec<String> {
    vec!["/bin/sh".into(), "-c".into(), command.into()]
}

async fn require_authorized(
    manager: &ExecPolicyManager,
    policy: &ScheduledShellPolicy,
    approval_policy: ApprovalPolicy,
) -> anyhow::Result<()> {
    let command = shell_command(&policy.command);
    match manager
        .create_exec_approval_requirement_for_command(ExecApprovalRequest {
            command: &command,
            approval_policy,
            vfs_policy: &policy.vfs_policy,
            sandbox_permissions: SandboxPermissions::UseDefault,
            prefix_rule: None,
        })
        .await
    {
        // Deliberately ignore bypass_sandbox: an immediate shell allow rule
        // must not become a recurring unsandboxed execution grant.
        ExecApprovalRequirement::Skip { .. } => Ok(()),
        ExecApprovalRequirement::Forbidden { reason } => bail!("{reason}"),
        ExecApprovalRequirement::NeedsApproval { .. } => {
            bail!(
                "recurring commands require a durable execpolicy allow rule, not one-shot approval"
            )
        }
    }
}

async fn load_current_config(home: &Path, cwd: &Path) -> anyhow::Result<Config> {
    Ok(ConfigBuilder::default()
        .chaos_home(home.to_path_buf())
        .fallback_cwd(Some(cwd.to_path_buf()))
        .build()
        .await?)
}

/// Issue a policy only for commands that can run unattended under disk-backed
/// permissions. Turn/session additional grants are deliberately not persisted.
pub(crate) async fn authorize(
    session: &Session,
    turn: &TurnContext,
    command: &str,
) -> anyhow::Result<String> {
    tokio::time::timeout(POLICY_TIMEOUT, async {
        if command.trim().is_empty() {
            bail!("scheduled command must not be empty");
        }
        if turn.network.is_some() {
            bail!("shell cron does not support managed network proxies");
        }
        let permissions = session.permission_snapshot(turn).await;
        let policy = ScheduledShellPolicy {
            version: POLICY_VERSION,
            command: command.to_string(),
            creator_session_id: session.conversation_id.to_string(),
            chaos_home: turn.config.chaos_home.clone(),
            cwd: turn.cwd.clone(),
            vfs_policy: permissions.vfs_policy,
            socket_policy: permissions.socket_policy,
            approval_policy: permissions.approval_policy,
        };
        let current = load_current_config(&policy.chaos_home, &policy.cwd).await?;
        policy.validate_config(&current)?;
        if turn.shell_environment_policy != current.permissions.shell_environment_policy {
            bail!("scheduled environment must match the config-file shell environment policy");
        }
        require_authorized(
            &session.services.exec_policy,
            &policy,
            policy.approval_policy,
        )
        .await?;
        check_current_rules(&policy, &current).await?;
        Ok(serde_json::to_string(&policy)?)
    })
    .await
    .context("scheduled authorization timed out")?
}

async fn check_current_rules(policy: &ScheduledShellPolicy, config: &Config) -> anyhow::Result<()> {
    // Unlike the interactive loader's warning/fallback path, malformed rules
    // are fatal for unattended work.
    let manager = ExecPolicyManager::new(Arc::new(
        load_exec_policy(&config.config_layer_stack).await?,
    ))
    .with_storage(&config.chaos_home, &policy.cwd);
    for approval in [
        policy.approval_policy,
        config.permissions.approval_policy.value(),
        ApprovalPolicy::Headless,
    ] {
        require_authorized(&manager, policy, approval).await?;
    }
    Ok(())
}

async fn prepare(
    launcher: &Config,
    job: &CronJob,
) -> anyhow::Result<(ScheduledShellPolicy, Config)> {
    let policy: ScheduledShellPolicy =
        serde_json::from_str(job.execution_policy.as_deref().context(
            "legacy shell job has no execution policy; recreate it through cron_create",
        )?)?;
    if policy.command != job.command
        || match job.scope {
            CronScope::Project => job.project_path.as_deref().map(Path::new) != Some(&policy.cwd),
            CronScope::Session | CronScope::Agent => {
                job.session_id.as_deref() != Some(policy.creator_session_id.as_str())
            }
        }
    {
        bail!("scheduled command or ownership differs from its authorization");
    }
    // Also honor the launching process's constraints (including CLI overrides),
    // not just the original owner's config. A different root cannot borrow a
    // saved project's write access.
    policy.validate_config(launcher)?;
    let current = load_current_config(&launcher.chaos_home, &policy.cwd).await?;
    policy.validate_config(&current)?;
    if current.permissions.shell_environment_policy != launcher.permissions.shell_environment_policy
    {
        bail!("scheduled environment differs from the launching process's constraints");
    }
    check_current_rules(&policy, &current).await?;
    check_current_rules(&policy, launcher).await?;
    Ok((policy, current))
}

pub(crate) fn executor(config: &Config) -> chaos_cron::JobExecutor {
    let launcher = Arc::new(config.clone());
    Arc::new(move |job| {
        let job = job.clone();
        let launcher = launcher.clone();
        Box::pin(async move {
            let (policy, current) = tokio::time::timeout(POLICY_TIMEOUT, prepare(&launcher, &job))
                .await
                .map_err(|_| "scheduled policy check timed out".to_string())?
                .map_err(|err| err.to_string())?;
            let output = process_exec_tool_call(
                ExecParams {
                    command: shell_command(&job.command),
                    cwd: policy.cwd.clone(),
                    expiration: ExecExpiration::Timeout(COMMAND_TIMEOUT),
                    env: create_env(&current.permissions.shell_environment_policy, None),
                    network: None,
                    sandbox_permissions: SandboxPermissions::UseDefault,
                    justification: None,
                    arg0: None,
                },
                &policy.vfs_policy,
                policy.socket_policy,
                &policy.cwd,
                &launcher.alcatraz_exe,
                None,
            )
            .await
            .map_err(|err| err.to_string())?;
            if output.timed_out {
                Err("scheduled command exceeded its 60s deadline".to_string())
            } else if output.exit_code != 0 {
                Err(format!(
                    "scheduled command exited with {}: {}",
                    output.exit_code, output.aggregated_output.text
                ))
            } else {
                Ok(output.aggregated_output.text)
            }
        })
    })
}

#[cfg(test)]
mod tests {
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
            serde_json::from_value::<chaos_cron::tools::create::CronCreateParams>(
                serde_json::json!({
                    "name": "injected", "schedule": {"kind": "interval", "seconds": 300},
                    "command": "echo cron", "execution_policy": job.execution_policy
                })
            )
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
}
