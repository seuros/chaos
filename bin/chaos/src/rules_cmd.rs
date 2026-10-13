//! User exec-policy rules stored in the runtime database.
use anyhow::{Context, bail};
use chaos_kern::user_settings::rules;
use std::path::PathBuf;

#[derive(Debug, usage::Args)]
pub struct RulesCommand {
    #[usage(subcommand)]
    pub action: RulesAction,
}

#[derive(Debug, usage::Subcommands)]
pub enum RulesAction {
    /// List stored rules.
    List,
    /// Print a stored rule document.
    Show { name: String },
    /// Validate and store a `.decrees` document. User rules can only restrict.
    Import {
        file: PathBuf,
        /// Name to store it under (default: the file stem).
        #[usage(long)]
        name: Option<String>,
    },
    /// Delete a stored rule.
    Remove { name: String },
}

pub async fn run(command: RulesCommand) -> anyhow::Result<()> {
    let home = chaos_pwd::find_chaos_home()?;
    chaos_kern::user_settings::install_persistence();
    match command.action {
        RulesAction::List => {
            let stored = rules::list_user_rules(&home).await?;
            if stored.is_empty() {
                println!("No stored rules. Import one with `chaos rules import <file.decrees>`.");
            }
            for rule in stored {
                println!("{}\t{} bytes", rule.name, rule.source.len());
            }
        }
        RulesAction::Show { name } => {
            let rule = rules::list_user_rules(&home)
                .await?
                .into_iter()
                .find(|rule| rule.name == name)
                .with_context(|| format!("no stored rule named `{name}`"))?;
            print!("{}", rule.source);
        }
        RulesAction::Import { file, name } => {
            let name = match name {
                Some(name) => name,
                None => file
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_owned)
                    .with_context(|| format!("{}: cannot derive a rule name", file.display()))?,
            };
            if file.extension().and_then(|ext| ext.to_str()) != Some("decrees") {
                bail!("{}: rule files must end in .decrees", file.display());
            }
            let source = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            rules::put_user_rule(&home, &name, &source).await?;
            println!("Stored rule `{name}`; new sessions apply it.");
        }
        RulesAction::Remove { name } => {
            rules::remove_user_rule(&home, &name).await?;
            println!("Removed rule `{name}`.");
        }
    }
    Ok(())
}
