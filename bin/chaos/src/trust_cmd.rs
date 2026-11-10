use anyhow::Context;
use chaos_kern::trust_admin::{self, Decision};

#[derive(Debug, usage::Args)]
pub struct TrustCommand {
    /// Authorize this exact operator change without a terminal prompt.
    /// Refused from agent shells: agents cannot grant themselves project trust.
    #[usage(long)]
    pub yes: bool,
    #[usage(subcommand)]
    pub action: Action,
}

#[derive(Debug, usage::Subcommands)]
pub enum Action {
    /// Print every stored decision with this installation's identity. Changes nothing.
    List,
    /// Print the stored decision for one project directory. Changes nothing.
    Show { project: std::path::PathBuf },
    /// Set the decision for one exact project directory.
    /// --expect names the decision you saw (trusted, untrusted or none); a different stored decision refuses.
    Set {
        project: std::path::PathBuf,
        /// trusted or untrusted
        decision: String,
        #[usage(long)]
        expect: String,
    },
    /// Deny one project directory. The denial is stored (untrusted), not deleted,
    /// so a manifest exported before the revocation cannot restore trust.
    Revoke {
        project: std::path::PathBuf,
        #[usage(long)]
        expect: String,
    },
    /// Print every decision, granted and denied, as a manifest bound to this installation.
    /// The manifest digest goes to stderr.
    Export,
    /// Print the digest of a manifest file. Changes nothing.
    Digest { file: std::path::PathBuf },
    /// Apply a reviewed manifest to this installation, all or nothing.
    Apply {
        file: std::path::PathBuf,
        /// The digest you reviewed (from `chaos trust digest` or `export`).
        #[usage(long)]
        digest: String,
    },
}

fn print_json(value: &impl serde::Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub async fn run(command: TrustCommand) -> anyhow::Result<()> {
    let home = chaos_pwd::find_chaos_home()?;
    let prepared = match command.action {
        Action::List => return print_json(&trust_admin::list(&home).await?),
        Action::Show { project } => {
            let key = trust_admin::normalize_project_path(&std::path::absolute(&project)?)?;
            let key = key.to_string_lossy();
            let listing = trust_admin::list(&home).await?;
            let decision = listing
                .entries
                .iter()
                .find(|entry| entry.project_path == key)
                .map_or(Decision::None, |entry| entry.decision);
            return print_json(&serde_json::json!({
                "installation_id": listing.installation_id,
                "project_path": key,
                "decision": decision,
            }));
        }
        Action::Export => {
            let manifest = trust_admin::export(&home).await?;
            eprintln!("digest: {}", manifest.digest()?);
            return print_json(&manifest);
        }
        Action::Digest { file } => {
            let manifest: trust_admin::Manifest =
                serde_json::from_str(&std::fs::read_to_string(&file)?)
                    .context("invalid project trust manifest")?;
            println!("{}", manifest.digest()?);
            return Ok(());
        }
        Action::Set {
            project,
            decision,
            expect,
        } => {
            let decision: Decision = decision.parse()?;
            anyhow::ensure!(
                decision != Decision::None,
                "a decision cannot be set to none; use `chaos trust revoke` to deny a project"
            );
            trust_admin::prepare_set(
                &home,
                &std::path::absolute(&project)?,
                decision,
                expect.parse()?,
            )
            .await?
        }
        Action::Revoke { project, expect } => {
            trust_admin::prepare_set(
                &home,
                &std::path::absolute(&project)?,
                Decision::Untrusted,
                expect.parse()?,
            )
            .await?
        }
        Action::Apply { file, digest } => {
            trust_admin::prepare_apply(&home, &std::fs::read_to_string(&file)?, &digest).await?
        }
    };
    print_json(&prepared.confirm_terminal(command.yes).await?)
}
