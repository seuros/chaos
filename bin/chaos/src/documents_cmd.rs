//! `chaos modes|prompts|themes|scripts`: named user documents in the runtime
//! database. One command shape per kind.
use anyhow::{Context, bail};
use chaos_kern::user_documents::{self, DocumentKind};
use std::path::PathBuf;

#[derive(Debug, usage::Args)]
pub struct DocumentsCommand {
    #[usage(subcommand)]
    pub action: DocumentsAction,
}

#[derive(Debug, usage::Subcommands)]
pub enum DocumentsAction {
    /// List stored documents.
    List,
    /// Show a stored document as JSON.
    Show { name: String },
    /// Validate and store files. The name is the file stem unless `--name` is given
    /// (one file only).
    Import {
        files: Vec<PathBuf>,
        #[usage(long)]
        name: Option<String>,
    },
    /// Print a stored document.
    Export { name: String },
    /// Delete a stored document.
    Remove { name: String },
    /// Make a stored document available to new sessions.
    Enable { name: String },
    /// Keep a stored document but hide it from new sessions.
    Disable { name: String },
}

pub async fn run(kind: DocumentKind, command: DocumentsCommand) -> anyhow::Result<()> {
    let home = chaos_pwd::find_chaos_home()?;
    chaos_kern::user_settings::install_persistence();
    let label = kind.as_str();
    match command.action {
        DocumentsAction::List => {
            let documents = user_documents::list(&home, kind).await?;
            if documents.is_empty() {
                println!("No stored {label}s. Import one with `chaos {label}s import <file>`.");
            }
            for document in documents {
                let state = if document.enabled {
                    "enabled"
                } else {
                    "disabled"
                };
                println!("{}\t{state}\trevision {}", document.name, document.revision);
            }
        }
        DocumentsAction::Show { name } => {
            let document = user_documents::get(&home, kind, &name)
                .await?
                .with_context(|| format!("no stored {label} named `{name}`"))?;
            println!("{}", serde_json::to_string_pretty(&document)?);
        }
        DocumentsAction::Export { name } => {
            let document = user_documents::get(&home, kind, &name)
                .await?
                .with_context(|| format!("no stored {label} named `{name}`"))?;
            print!("{}", document.source);
        }
        DocumentsAction::Import { files, name } => {
            if files.is_empty() {
                bail!("give at least one file to import");
            }
            if name.is_some() && files.len() > 1 {
                bail!("--name applies to a single file");
            }
            for file in files {
                let stem = file
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_owned);
                let name = name
                    .clone()
                    .or(stem)
                    .with_context(|| format!("{}: cannot derive a name", file.display()))?;
                let source = std::fs::read_to_string(&file)
                    .with_context(|| format!("failed to read {}", file.display()))?;
                let revision = user_documents::import(&home, kind, &name, &source).await?;
                println!("Stored {label} `{name}` (revision {revision}); new sessions see it.");
            }
        }
        DocumentsAction::Remove { name } => {
            user_documents::remove(&home, kind, &name).await?;
            println!("Removed {label} `{name}`.");
        }
        DocumentsAction::Enable { name } => {
            user_documents::set_enabled(&home, kind, &name, true).await?;
            println!("{label} `{name}` is now enabled; new sessions see it.");
        }
        DocumentsAction::Disable { name } => {
            user_documents::set_enabled(&home, kind, &name, false).await?;
            println!("{label} `{name}` is now disabled; new sessions skip it.");
        }
    }
    Ok(())
}
