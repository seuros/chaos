use chaos_kern::user_settings;
use chaos_proc::planning::{PlanMutation, PlanningActor};

#[derive(Debug, usage::Args)]
pub struct WorkspaceCommand {
    #[usage(subcommand)]
    action: WorkspaceAction,
}

#[derive(Debug, usage::Subcommands)]
enum WorkspaceAction {
    List {
        #[usage(long, default = "0")]
        offset: i64,
    },
    Projects {
        workspace: String,
        #[usage(long, default = "0")]
        offset: i64,
    },
    Create {
        name: String,
    },
    Rename {
        workspace: String,
        revision: i16,
        name: String,
    },
    RegisterProject {
        workspace: String,
        name: String,
    },
    Bind {
        project: String,
        path: std::path::PathBuf,
    },
    Locate,
}

fn print(value: impl serde::Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

pub async fn workspace(command: WorkspaceCommand) -> anyhow::Result<()> {
    let home = chaos_pwd::find_chaos_home()?;
    let db = user_settings::open(&home).await?;
    let installation = user_settings::installation_id(&home)?;
    match command.action {
        WorkspaceAction::List { offset } => print(db.planning_workspaces(offset).await?),
        WorkspaceAction::Projects { workspace, offset } => {
            print(db.planning_projects(&workspace, offset).await?)
        }
        WorkspaceAction::Create { name } => print(db.planning_create_workspace(&name).await?),
        WorkspaceAction::Rename {
            workspace,
            revision,
            name,
        } => print(
            db.planning_rename_workspace(&workspace, revision, &name)
                .await?,
        ),
        WorkspaceAction::RegisterProject { workspace, name } => {
            print(db.planning_register_project(&workspace, &name).await?)
        }
        WorkspaceAction::Bind { project, path } => {
            print(db.planning_bind(&installation, &project, &path).await?)
        }
        WorkspaceAction::Locate => print(
            db.planning_checkout(&installation, &std::env::current_dir()?)
                .await?,
        ),
    }
}

#[derive(Debug, usage::Args)]
pub struct PlanCommand {
    #[usage(subcommand)]
    action: PlanAction,
}

#[derive(Debug, usage::Subcommands)]
enum PlanAction {
    List {
        workspace: String,
        #[usage(long, default = "0")]
        offset: i64,
    },
    Read {
        plan: String,
        #[usage(long, default = "0")]
        offset: i64,
    },
    Task {
        plan: String,
        task: String,
        #[usage(long, default = "0")]
        offset: i64,
    },
    History {
        plan: String,
        #[usage(long, default = "0")]
        after: i64,
    },
    Attach {
        session: String,
        plan: String,
    },
    Detach {
        session: String,
    },
    /// JSON mutation; request_id deduplicates retries.
    Change {
        json: String,
    },
    Graph {
        plan: String,
        #[usage(long)]
        task: Option<String>,
        #[usage(long, default = "0")]
        offset: i64,
    },
    ValidateGraph {
        plan: String,
        #[usage(long, default = "0")]
        offset: i64,
    },
    RebuildGraph {
        plan: String,
        revision: i16,
    },
}

pub async fn plan(command: PlanCommand) -> anyhow::Result<()> {
    let home = chaos_pwd::find_chaos_home()?;
    let db = user_settings::open(&home).await?;
    let installation = user_settings::installation_id(&home)?;
    match command.action {
        PlanAction::List { workspace, offset } => {
            print(db.planning_list(&workspace, offset).await?)
        }
        PlanAction::Read { plan, offset } => print(db.planning_read(&plan, offset).await?),
        PlanAction::Task { plan, task, offset } => {
            print(db.planning_task(&plan, &task, offset).await?)
        }
        PlanAction::History { plan, after } => print(db.planning_history(&plan, after).await?),
        PlanAction::Graph { plan, task, offset } => {
            print(db.planning_graph(&plan, task.as_deref(), offset).await?)
        }
        PlanAction::ValidateGraph { plan, offset } => {
            print(db.planning_validate_graph(&plan, offset).await?)
        }
        PlanAction::RebuildGraph { plan, revision } => {
            db.planning_rebuild_graph(&plan, revision).await?;
            print(db.planning_validate_graph(&plan, 0).await?)
        }
        PlanAction::Attach { session, plan } => {
            db.planning_attach(&session, Some(&plan)).await?;
            print(db.planning_attachment(&session).await?)
        }
        PlanAction::Detach { session } => {
            db.planning_attach(&session, None).await?;
            print(Option::<String>::None)
        }
        PlanAction::Change { json } => {
            let request: PlanMutation = serde_json::from_str(&json)?;
            print(
                db.planning_mutate(
                    &PlanningActor {
                        session: format!("operator:{installation}"),
                        installation,
                    },
                    &request,
                )
                .await?,
            )
        }
    }
}
