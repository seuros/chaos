//! Agent role definitions stored in the runtime database.
use anyhow::{Context, bail};
use chaos_kern::user_settings;
use std::path::{Path, PathBuf};

#[derive(Debug, usage::Args)]
pub struct AgentsCommand {
    #[usage(subcommand)]
    pub action: AgentsAction,
}

#[derive(Debug, usage::Subcommands)]
pub enum AgentsAction {
    /// List stored roles.
    List,
    /// Show a stored role as JSON.
    Show { name: String },
    /// Validate and store role documents (`.toml`, or `.md` with TOML front matter).
    /// The role name comes from the document; relative paths resolve against the
    /// file's directory.
    Import { files: Vec<PathBuf> },
    /// Print a stored role document.
    Export { name: String },
    /// Delete a stored role.
    Remove { name: String },
    /// Make a stored role available to new sessions.
    Enable { name: String },
    /// Keep a stored role but hide it from new sessions.
    Disable { name: String },
}

pub async fn run(command: AgentsCommand) -> anyhow::Result<()> {
    let home = chaos_pwd::find_chaos_home()?;
    let runtime = user_settings::open(&home).await?;
    match command.action {
        AgentsAction::List => {
            let roles = runtime.list_agent_roles().await?;
            if roles.is_empty() {
                println!("No stored agent roles. Import one with `chaos agents import <file>`.");
            }
            for role in roles {
                let state = if role.enabled { "enabled" } else { "disabled" };
                println!("{}\t{}\trevision {}", role.name, state, role.revision);
            }
        }
        AgentsAction::Show { name } => {
            let role = runtime
                .get_agent_role(&name)
                .await?
                .with_context(|| format!("no stored agent role named `{name}`"))?;
            println!("{}", serde_json::to_string_pretty(&role)?);
        }
        AgentsAction::Export { name } => {
            let role = runtime
                .get_agent_role(&name)
                .await?
                .with_context(|| format!("no stored agent role named `{name}`"))?;
            print!("{}", role.source);
        }
        AgentsAction::Import { files } => {
            if files.is_empty() {
                bail!("give at least one role file to import");
            }
            for file in files {
                let (name, revision) = import_role(&runtime, &file).await?;
                println!("Stored agent role `{name}` (revision {revision}); new sessions see it.");
            }
        }
        AgentsAction::Remove { name } => {
            let role = runtime
                .get_agent_role(&name)
                .await?
                .with_context(|| format!("no stored agent role named `{name}`"))?;
            runtime.delete_agent_role(&name, role.revision).await?;
            println!("Removed agent role `{name}`.");
        }
        AgentsAction::Enable { name } => set_enabled(&runtime, &name, true).await?,
        AgentsAction::Disable { name } => set_enabled(&runtime, &name, false).await?,
    }
    Ok(())
}

async fn import_role(
    runtime: &chaos_proc::RuntimeDbHandle,
    file: &Path,
) -> anyhow::Result<(String, i64)> {
    let format = match file.extension().and_then(|ext| ext.to_str()) {
        Some("toml") => "toml",
        Some("md") => "md",
        _ => bail!(
            "{}: agent role files must end in .toml or .md",
            file.display()
        ),
    };
    let source = std::fs::read_to_string(file)
        .with_context(|| format!("failed to read {}", file.display()))?;
    let base_dir = file
        .canonicalize()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    let parsed = chaos_sysctl::agent_roles::parse_agent_role_file_contents(
        &source,
        file,
        base_dir.as_deref().unwrap_or_else(|| Path::new(".")),
        /*require_developer_instructions*/ true,
    )?;
    let revision = runtime
        .get_agent_role(&parsed.role_name)
        .await?
        .map(|record| record.revision)
        .unwrap_or(0);
    let base_dir = base_dir.map(|dir| dir.to_string_lossy().into_owned());
    let revision = runtime
        .put_agent_role(
            &parsed.role_name,
            format,
            &source,
            base_dir.as_deref(),
            revision,
        )
        .await?;
    Ok((parsed.role_name, revision))
}

async fn set_enabled(
    runtime: &chaos_proc::RuntimeDbHandle,
    name: &str,
    enabled: bool,
) -> anyhow::Result<()> {
    let role = runtime
        .get_agent_role(name)
        .await?
        .with_context(|| format!("no stored agent role named `{name}`"))?;
    runtime
        .set_agent_role_enabled(name, role.revision, enabled)
        .await?;
    println!(
        "Agent role `{name}` is now {}; new sessions see the change.",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}
