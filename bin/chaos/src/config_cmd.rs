//! Recovery commands intentionally bypass ConfigBuilder and runtime startup.
use anyhow::Context;
use chaos_kern::user_settings;
use std::path::PathBuf;

#[derive(Debug, usage::Args)]
pub struct ConfigCommand {
    #[usage(subcommand)]
    pub action: ConfigAction,
}

#[derive(Debug, usage::Subcommands)]
pub enum ConfigAction {
    /// Inspect database settings (optional dotted key).
    Get { key: Option<String> },
    /// Set a user setting using a JSON value.
    Set { key: String, value: String },
    /// Remove a user setting.
    Unset { key: String },
    /// Export user settings as TOML; grants and secret values are excluded.
    Export,
    /// Replace user settings from TOML using a revision-checked write.
    Import { file: PathBuf },
    /// Explicitly migrate legacy user TOML; old grants require reapproval.
    Migrate {
        #[usage(long)]
        dry_run: bool,
    },
    /// Diagnose bootstrap/storage without starting providers or MCP servers.
    Doctor,
    /// Inspect or edit bootstrap settings without opening storage.
    Bootstrap(BootstrapCommand),
}

#[derive(Debug, usage::Args)]
pub struct BootstrapCommand {
    #[usage(subcommand)]
    pub action: BootstrapAction,
}

#[derive(Debug, usage::Subcommands)]
pub enum BootstrapAction {
    Show,
    Set { key: String, value: String },
}

#[derive(Debug, usage::Args)]
pub struct ApprovalsCommand {
    #[usage(subcommand)]
    pub action: ApprovalsAction,
}

#[derive(Debug, usage::Subcommands)]
pub enum ApprovalsAction {
    List,
    /// Revoke a grant ID or all installation-local grants.
    Revoke {
        id: String,
    },
}

pub async fn run(command: ConfigCommand) -> anyhow::Result<()> {
    let home = chaos_pwd::find_chaos_home()?;
    user_settings::install_persistence();
    match command.action {
        ConfigAction::Migrate { dry_run } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&user_settings::migrate(&home, dry_run).await?)?
            );
        }
        ConfigAction::Bootstrap(command) => match command.action {
            BootstrapAction::Show => {
                let bootstrap = user_settings::BootstrapConfig::read(&home)?.redacted();
                println!("{}", toml::to_string_pretty(&bootstrap)?);
            }
            BootstrapAction::Set { key, value } => {
                user_settings::set_bootstrap(&home, &key, &value)?;
                println!("Bootstrap updated; restart ChaOS to use it.");
            }
        },
        ConfigAction::Doctor => {
            user_settings::BootstrapConfig::read(&home)?;
            println!("Bootstrap: valid");
            let snapshot = user_settings::snapshot(&home).await?;
            println!("Database settings: revision {}", snapshot.revision);
        }
        ConfigAction::Get { key } => {
            let snapshot = user_settings::snapshot(&home).await?;
            let mut value = &snapshot.settings;
            if let Some(key) = key {
                for segment in key.split('.') {
                    value = value.get(segment).context("setting not found")?;
                }
            }
            println!("{}", serde_json::to_string_pretty(value)?);
        }
        ConfigAction::Export => {
            println!(
                "{}",
                toml::to_string_pretty(&user_settings::snapshot(&home).await?.settings)?
            );
        }
        ConfigAction::Import { file } => {
            let settings: toml::Value = toml::from_str(&std::fs::read_to_string(file)?)?;
            let snapshot = user_settings::snapshot(&home).await?;
            chaos_sysctl::persistence::backend()?
                .commit(&home, snapshot.revision, settings)
                .await?;
            println!("User settings imported; approvals were not imported.");
        }
        ConfigAction::Set { key, value } => {
            let value: serde_json::Value =
                serde_json::from_str(&value).context("value must be JSON")?;
            let value: toml::Value = serde_json::from_value(value)?;
            let doc = toml::to_string(&std::collections::BTreeMap::from([("value", value)]))?
                .parse::<toml_edit::DocumentMut>()?;
            chaos_sysctl::edit::ConfigEditsBuilder::new(&home)
                .with_edits([chaos_sysctl::edit::ConfigEdit::SetPath {
                    segments: key.split('.').map(str::to_owned).collect(),
                    value: doc["value"].clone(),
                }])
                .apply()
                .await?;
        }
        ConfigAction::Unset { key } => {
            chaos_sysctl::edit::ConfigEditsBuilder::new(&home)
                .with_edits([chaos_sysctl::edit::ConfigEdit::ClearPath {
                    segments: key.split('.').map(str::to_owned).collect(),
                }])
                .apply()
                .await?;
        }
    }
    Ok(())
}

pub async fn approvals(command: ApprovalsCommand) -> anyhow::Result<()> {
    let home = chaos_pwd::find_chaos_home()?;
    let runtime = user_settings::open(&home).await?;
    let installation = user_settings::installation_id(&home)?;
    match command.action {
        ApprovalsAction::List => {
            println!(
                "{}",
                serde_json::to_string_pretty(&runtime.list_approvals(&installation).await?)?
            );
        }
        ApprovalsAction::Revoke { id } => {
            let count = runtime
                .revoke_approvals(&installation, (id != "all").then_some(id.as_str()))
                .await?;
            println!("Revoked {count} grants. Matching session approvals are invalidated.");
        }
    }
    Ok(())
}
