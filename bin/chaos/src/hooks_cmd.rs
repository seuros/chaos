use chaos_ipc::hooks::HookDefinition;
use chaos_kern::hooks::{self, HookAction, HookChangeRequest};

#[derive(Debug, usage::Args)]
pub struct HooksCommand {
    /// Authorize this exact operator mutation without a terminal prompt.
    /// Not available from agent shells; agents use hooks_* tools.
    #[usage(long)]
    pub yes: bool,
    #[usage(subcommand)]
    pub action: Action,
}

#[derive(Debug, usage::Subcommands)]
pub enum Action {
    /// Explicit one-time import; all hooks start disabled. No runtime file fallback.
    Import {
        file: std::path::PathBuf,
        #[usage(long)]
        prefix: String,
        #[usage(long)]
        project: bool,
    },
    List,
    Show {
        id: String,
    },
    /// Register a disabled hook; --enabled also authorizes recurring execution.
    Add {
        id: String,
        #[usage(long)]
        event: String,
        #[usage(long)]
        command: String,
        #[usage(long)]
        project: bool,
        #[usage(long)]
        enabled: bool,
    },
    /// Replace the definition using a JSON object (not a configuration file).
    Update {
        id: String,
        definition: String,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    Remove {
        id: String,
    },
}

pub async fn run(command: HooksCommand) -> anyhow::Result<()> {
    let home = chaos_pwd::find_chaos_home()?;
    let cwd = std::env::current_dir()?;
    let mut definition = None;
    let mut enabled = None;
    let (id, action) = match command.action {
        Action::Import {
            file,
            prefix,
            project,
        } => {
            hooks::import_terminal(&home, &cwd, &file, &prefix, project, command.yes).await?;
            println!("Imported disabled hooks. Enable each hook to authorize execution.");
            return Ok(());
        }
        Action::List => {
            println!(
                "{}",
                hooks::resource_json(&home, &cwd, None)
                    .await
                    .map_err(anyhow::Error::msg)?
            );
            return Ok(());
        }
        Action::Show { id } => {
            println!(
                "{}",
                hooks::resource_json(&home, &cwd, Some(&id))
                    .await
                    .map_err(anyhow::Error::msg)?
            );
            return Ok(());
        }
        Action::Add {
            id,
            event,
            command,
            project,
            enabled: activate,
        } => {
            definition = Some(serde_json::from_value::<HookDefinition>(
                serde_json::json!({
                    "event": event.replace('-', "_"), "command": command,
                    "project": project.then_some("."),
                }),
            )?);
            enabled = Some(activate);
            (id, HookAction::Create)
        }
        Action::Update {
            id,
            definition: value,
        } => {
            definition = Some(serde_json::from_str(&value)?);
            (id, HookAction::Update)
        }
        Action::Enable { id } => (id, HookAction::Enable),
        Action::Disable { id } => (id, HookAction::Disable),
        Action::Remove { id } => (id, HookAction::Delete),
    };
    let expected_revision = hooks::list(&home, &cwd)
        .await?
        .into_iter()
        .find(|h| h.hook.id == id)
        .map(|h| h.hook.revision);
    let change = hooks::prepare(
        &home,
        &cwd,
        HookChangeRequest {
            id,
            action,
            expected_revision,
            definition,
            enabled,
        },
    )
    .await?;
    let revision = change.confirm_terminal(command.yes).await?;
    println!("Hooks updated (revision {revision}); effective at the next hook event.");
    Ok(())
}
