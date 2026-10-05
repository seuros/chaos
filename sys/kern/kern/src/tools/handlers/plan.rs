use std::collections::BTreeMap;

use chaos_ipc::protocol::EventMsg;
use chaos_parrot::sanitize::JsonSchema;
use chaos_proc::RuntimeDbHandle;
use chaos_proc::planning::{
    ConsolidationRequest, PlanChange, PlanMutation, PlanSnapshot, PlanningActor,
};
use serde::{Deserialize, Serialize};

use super::{extract_function_arguments, parse_arguments};
use crate::chaos::{Session, TurnContext};
use crate::client_common::tools::{ResponsesApiTool, ToolSpec};
use crate::function_tool::FunctionCallError;
use crate::tools::context::{FunctionToolOutput, ToolInvocation};
use crate::tools::registry::{ToolHandler, ToolKind};

pub struct PlanHandler;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    List,
    Projects,
    Read,
    Detail,
    Task,
    Clarifications,
    Consolidate,
    Consolidation,
    History,
    Graph,
    Attach,
    Detach,
    Change,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    action: Operation,
    plan: Option<String>,
    workspace: Option<String>,
    task: Option<String>,
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    after: i64,
    request_id: Option<String>,
    expected_revision: Option<i16>,
    change: Option<PlanChange>,
    provider: Option<String>,
    model: Option<String>,
    job: Option<String>,
}

fn error(e: impl std::fmt::Display) -> FunctionCallError {
    FunctionCallError::RespondToModel(e.to_string())
}

fn output(value: impl Serialize) -> Result<FunctionToolOutput, FunctionCallError> {
    Ok(FunctionToolOutput::from_text(
        serde_json::to_string(&value).map_err(error)?,
        Some(true),
    ))
}

#[derive(Serialize)]
struct Attachment {
    attached_plan: Option<String>,
}

async fn read_and_publish(
    session: &Session,
    turn: &TurnContext,
    db: &RuntimeDbHandle,
    plan: &str,
    offset: i64,
) -> Result<PlanSnapshot, FunctionCallError> {
    let snapshot = db.planning_read(plan, offset).await.map_err(error)?;
    session
        .send_transient_event(turn, EventMsg::PlanUpdate(snapshot.clone().into()))
        .await;
    Ok(snapshot)
}

pub(crate) fn planning_tool(
    authoring: bool,
    planning: chaos_proc::planning::PlanningCapabilities,
) -> ToolSpec {
    let string = |s: &str| JsonSchema::String {
        description: Some(s.into()),
    };
    let integer = |s: &str| JsonSchema::Integer {
        description: Some(s.into()),
    };
    let mut changes = vec!["transition", "note", "complete", "cancel"];
    let mut actions = vec![
        "read",
        "detail",
        "task",
        "clarifications",
        "consolidation",
        "history",
        "detach",
        "change",
    ];
    let mut task_description = if authoring {
        "Task for details, clarifications or consolidation; omit for plan content."
    } else {
        "Task for details or clarifications; omit for plan clarifications."
    }
    .to_owned();
    if authoring {
        changes.extend([
            "create",
            "edit",
            "add_task",
            "edit_task",
            "move_task",
            "clarify",
            "reopen",
        ]);
        actions.extend(["consolidate", "list", "projects", "attach"]);
        if planning.dependencies {
            changes.extend(["link", "unlink"]);
        }
    }
    if planning.dependencies {
        actions.push("graph");
        task_description.push_str(" For graph: optional descendant root.");
    }
    let mut change = BTreeMap::from([
        ("action".into(), string(&changes.join(", "))),
        (
            "task".into(),
            string(
                "Task UUID, T-number in the attached plan, or qualified workspace/plan/T-number.",
            ),
        ),
        (
            "task_revision".into(),
            integer("Required for transition/edit_task/move_task. Read current revision first."),
        ),
        (
            "event".into(),
            string("Task transition: start, block, unblock, complete, cancel, reopen."),
        ),
        (
            "reason".into(),
            string("Required explanation for transitions and plan lifecycle changes."),
        ),
        (
            "note".into(),
            string("Append-only progress note; task is optional."),
        ),
    ]);
    if authoring {
        change.extend([
            ("workspace".into(), string("Workspace UUID or six-hex code, required for create.")),
            ("title".into(), string("Title for create/edit/add_task/edit_task.")),
            ("body".into(), string("Initial Markdown body for create/add_task; omitted means empty. Use clarify for subsequent changes.")),
            ("text".into(), string("Required for clarify: an appended intent change. Omit task to clarify the plan.")),
            ("parent".into(), string(if planning.dependencies {
                "Parent task for nesting or prerequisite task for a dependency. Omit for root nesting."
            } else { "Parent task for nesting. Omit for root nesting." })),
            ("position".into(), integer("Required for add_task/move_task; display order, unrelated to task identity.")),
            ("projects".into(), JsonSchema::Array { description: Some("Required for edit_task: project UUIDs in this workspace.".into()), items: Box::new(string("Project UUID")) }),
        ]);
        if planning.dependencies {
            change.insert("child".into(), string("Dependent task for link/unlink."));
        }
    }
    let mut properties = BTreeMap::from([
        ("action".into(), string(&actions.join(", "))),
        ("task".into(), string(&task_description)),
        (
            "offset".into(),
            integer("Page offset, default 0. At most 50 records per page."),
        ),
        (
            "after".into(),
            integer("Exclusive history or clarification sequence returned by the previous page."),
        ),
        (
            "request_id".into(),
            string(if authoring {
                "Required for change/consolidate. Unique retry key; reuse only with identical arguments."
            } else {
                "Required for change. Unique retry key; reuse only with identical arguments."
            }),
        ),
        (
            "expected_revision".into(),
            integer("Current plan revision for structural or plan lifecycle changes."),
        ),
        (
            "change".into(),
            JsonSchema::Object {
                properties: change,
                required: Some(vec!["action".into()]),
                additional_properties: Some(false.into()),
            },
        ),
    ]);
    if authoring {
        properties.insert(
            "provider".into(),
            string("Required for consolidate: select a configured provider from chaos://models."),
        );
        properties.insert(
            "model".into(),
            string("Required for consolidate: select a model from that provider's catalogue."),
        );
        properties.insert(
            "plan".into(),
            string(
                "Plan UUID or six-hex/four-hex reference. Defaults to explicit session attachment.",
            ),
        );
        properties.insert(
            "workspace".into(),
            string(
                "For list/projects; otherwise inferred from this installation's checkout binding.",
            ),
        );
    }
    properties.insert("job".into(), string("Job ID for consolidation status. Cancellation uses cancel_mcp_task and the returned background task ID."));
    ToolSpec::Function(ResponsesApiTool {
        name: if authoring { "plan" } else { "plan_progress" }.into(),
        description: if authoring {
            "Author durable database plans. Attach explicitly before execution. Read bounded pages; IDs are immutable, ordering is separate. After replanning, switch back to execution mode yourself."
        } else {
            "Read and update the explicitly attached database plan. Read task bodies and pending clarifications before execution. Revision conflicts require re-reading, not blind retries. For intent or structural changes, switch to Plan mode yourself, revise, then return to execution."
        }.into(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object { properties, required: Some(vec!["action".into()]), additional_properties: Some(false.into()) },
        output_schema: None,
    })
}

impl ToolHandler for PlanHandler {
    type Output = FunctionToolOutput;
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }
    fn is_mutating(
        &self,
        _invocation: &ToolInvocation,
    ) -> impl std::future::Future<Output = bool> + Send + '_ {
        std::future::ready(true)
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        // Recheck the current mode and attachment, not merely the schema that
        // happened to be visible when this invocation was produced.
        let turn = invocation
            .session
            .effective_turn_context(&invocation.turn)
            .await;
        let authoring = turn.tools_config.planning_authoring;
        if !turn.mode_capabilities.planning_records
            || !turn.tools_config.planning.available
            || (invocation.tool_name == "plan" && !authoring)
            || (invocation.tool_name == "plan_progress" && authoring)
        {
            return Err(error("planning tool is not available in the current mode"));
        }
        let db = invocation
            .session
            .runtime_db()
            .ok_or_else(|| error("planning database unavailable"))?;
        let session = invocation.session.conversation_id.to_string();
        let attached = db.planning_attachment(&session).await.map_err(error)?;
        if !authoring && attached.is_none() {
            return Err(error("no plan is attached"));
        }
        let args: Args = parse_arguments(&extract_function_arguments(
            invocation.payload,
            &invocation.tool_name,
        )?)?;
        let uses_attached_plan = args.action == Operation::Detach
            || (args.plan.is_none()
                && !matches!(args.action, Operation::List | Operation::Projects)
                && !matches!(&args.change, Some(PlanChange::Create { .. })));
        if uses_attached_plan && invocation.turn.tools_config.attached_plan != attached {
            return Err(error(
                "attachment_conflict: the selected plan changed; refresh before continuing",
            ));
        }
        if !authoring && (args.plan.is_some() || args.workspace.is_some()) {
            return Err(error(
                "progress operations are restricted to the attached plan",
            ));
        }
        let installation =
            crate::user_settings::installation_id(&turn.config.chaos_home).map_err(error)?;
        let actor = PlanningActor {
            session: session.clone(),
            installation: installation.clone(),
        };
        let use_attachment = args.plan.is_none();
        let plan = args.plan.as_deref().or(attached.as_deref());
        let require_plan =
            || plan.ok_or_else(|| error("select an explicit plan or attach one first"));
        match args.action {
            Operation::List | Operation::Projects if authoring => {
                let workspace = match args.workspace {
                    Some(w) => w,
                    None => db.planning_checkout(&installation, &turn.cwd).await.map_err(error)?
                        .map(|checkout|checkout.workspace_id)
                        .ok_or_else(|| error("no checkout binding; operator must register workspace/projects or supply workspace ID"))?,
                };
                if args.action == Operation::List {
                    output(
                        db.planning_list(&workspace, args.offset)
                            .await
                            .map_err(error)?,
                    )
                } else {
                    output(
                        db.planning_projects(&workspace, args.offset)
                            .await
                            .map_err(error)?,
                    )
                }
            }
            Operation::Read => output(
                read_and_publish(
                    &invocation.session,
                    &turn,
                    &db,
                    require_plan()?,
                    args.offset,
                )
                .await?,
            ),
            Operation::Task => output(
                db.planning_task(
                    require_plan()?,
                    args.task
                        .as_deref()
                        .ok_or_else(|| error("task is required"))?,
                    args.offset,
                )
                .await
                .map_err(error)?,
            ),
            Operation::Detail => output(db.planning_detail(require_plan()?).await.map_err(error)?),
            Operation::Consolidate if authoring => {
                let request = ConsolidationRequest {
                    request_id: args
                        .request_id
                        .ok_or_else(|| error("request_id is required"))?,
                    plan: require_plan()?.into(),
                    task: args.task,
                    provider: args
                        .provider
                        .ok_or_else(|| error("provider is required; select from chaos://models"))?,
                    model: args
                        .model
                        .ok_or_else(|| error("model is required; select from chaos://models"))?,
                };
                let job = invocation
                    .session
                    .request_consolidation(
                        turn,
                        &actor,
                        &request,
                        use_attachment.then_some(require_plan()?),
                        &invocation.call_id,
                    )
                    .await
                    .map_err(error)?;
                output(serde_json::json!({
                    "job": job,
                    "task_id": crate::background_tasks::TaskRegistry::submission_id(&job.origin_call_id),
                }))
            }
            Operation::Consolidation => {
                let job = db
                    .planning_consolidation(
                        args.job
                            .as_deref()
                            .ok_or_else(|| error("job is required"))?,
                    )
                    .await
                    .map_err(error)?;
                let plan = db
                    .planning_resolve_plan(require_plan()?)
                    .await
                    .map_err(error)?;
                if job.plan_id != plan {
                    return Err(error("job belongs to another plan"));
                }
                output(job)
            }
            Operation::Clarifications => output(
                db.planning_clarifications(require_plan()?, args.task.as_deref(), args.after)
                    .await
                    .map_err(error)?,
            ),
            Operation::History => output(
                db.planning_history(require_plan()?, args.after)
                    .await
                    .map_err(error)?,
            ),
            Operation::Graph => output(
                db.planning_graph(require_plan()?, args.task.as_deref(), args.offset)
                    .await
                    .map_err(error)?,
            ),
            Operation::Attach if authoring => {
                if use_attachment {
                    return Err(error("attach requires an explicit plan"));
                }
                db.planning_attach(&session, Some(require_plan()?))
                    .await
                    .map_err(error)?;
                read_and_publish(&invocation.session, &turn, &db, require_plan()?, 0).await?;
                output(Attachment {
                    attached_plan: db.planning_attachment(&session).await.map_err(error)?,
                })
            }
            Operation::Detach => {
                db.planning_detach(
                    &session,
                    attached
                        .as_deref()
                        .ok_or_else(|| error("no plan is attached"))?,
                )
                .await
                .map_err(error)?;
                output(Attachment {
                    attached_plan: None,
                })
            }
            Operation::Change => {
                let change = args.change.ok_or_else(|| error("change is required"))?;
                if !authoring && !change.progress_only() {
                    return Err(error(
                        "intent and structural edits require Plan mode; switch_mode yourself, edit, then return",
                    ));
                }
                let mutation = PlanMutation {
                    request_id: args
                        .request_id
                        .ok_or_else(|| error("request_id is required"))?,
                    plan: if matches!(change, PlanChange::Create { .. }) {
                        None
                    } else {
                        Some(require_plan()?.into())
                    },
                    expected_revision: args.expected_revision,
                    change,
                };
                let result = if use_attachment && mutation.plan.is_some() {
                    db.planning_mutate_attached(&actor, &mutation, require_plan()?)
                        .await
                } else {
                    db.planning_mutate(&actor, &mutation).await
                }
                .map_err(error)?;
                // Never publish proposed intent as if it committed. Read the
                // current database view only after the mutation succeeded.
                read_and_publish(&invocation.session, &turn, &db, &result.plan.id, 0).await?;
                output(result)
            }
            _ => Err(error("action is unavailable in this mode")),
        }
    }
}

#[cfg(test)]
mod integration_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_proc::planning::PlanningCapabilities;

    #[test]
    fn schemas_only_advertise_backend_capabilities() -> serde_json::Result<()> {
        for authoring in [false, true] {
            let sqlite =
                serde_json::to_string(&planning_tool(authoring, PlanningCapabilities::SQLITE))?;
            let postgres =
                serde_json::to_string(&planning_tool(authoring, PlanningCapabilities::POSTGRES))?;
            assert!(!sqlite.contains("graph"));
            assert!(!sqlite.contains("unlink"));
            assert!(!sqlite.contains("PostgreSQL"));
            assert!(!sqlite.contains("\"child\""));
            assert!(postgres.contains("graph"));
            assert!(!postgres.contains("SQLite"));
            assert!(!postgres.contains("PostgreSQL only"));
            assert_eq!(postgres.contains("unlink"), authoring);
            assert_eq!(postgres.contains("\"provider\""), authoring);
            assert_eq!(postgres.contains("\"body\""), authoring);
            assert_eq!(postgres.contains("\"text\""), authoring);
            assert_eq!(postgres.contains("consolidate,"), authoring);
            assert!(postgres.contains("clarifications"));
            assert!(postgres.contains("consolidation"));
        }
        Ok(())
    }
}
