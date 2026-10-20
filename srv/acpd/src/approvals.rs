use std::sync::Arc;

use acp_host::{SessionContext, protocol};
use chaos_ipc::protocol::{
    ApplyPatchApprovalRequestEvent, ExecApprovalRequestEvent, Op, ReviewDecision,
};
use chaos_kern::Process;
use protocol::schema::v1::{
    PermissionOption, PermissionOptionKind, RequestPermissionOutcome, ToolCallStatus,
    ToolCallUpdate, ToolKind,
};

use crate::{projection::Projection, session::submit};

async fn decision(
    context: &SessionContext,
    update: ToolCallUpdate,
    may_allow: bool,
) -> ReviewDecision {
    let mut options = Vec::new();
    if may_allow {
        options.push(PermissionOption::new(
            "allow",
            "Allow once",
            PermissionOptionKind::AllowOnce,
        ));
    }
    options.push(PermissionOption::new(
        "deny",
        "Reject once",
        PermissionOptionKind::RejectOnce,
    ));
    let result = context.client().request_permission(update, options).await;
    if may_allow
        && !context.is_cancelled()
        && matches!(result, Ok(response)
            if matches!(response.outcome, RequestPermissionOutcome::Selected(ref selected)
                if selected.option_id.0.as_ref() == "allow"))
    {
        ReviewDecision::Approved
    } else {
        ReviewDecision::Denied
    }
}

pub(crate) async fn exec(
    process: &Arc<Process>,
    context: &SessionContext,
    projection: &mut Projection,
    event: &ExecApprovalRequestEvent,
) -> protocol::Result<()> {
    let update = projection.tool(
        context,
        &event.call_id,
        event.command.join(" "),
        ToolKind::Execute,
        ToolCallStatus::Pending,
        Some(serde_json::json!({"command":event.command,"cwd":event.cwd,"reason":event.reason})),
    )?;
    let may_allow = event
        .effective_available_decisions()
        .contains(&ReviewDecision::Approved);
    let decision = decision(context, update, may_allow).await;
    submit(
        process,
        Op::ExecApproval {
            id: event.effective_approval_id(),
            turn_id: Some(event.turn_id.clone()),
            decision,
        },
        "native exec approval delivery failed",
    )
    .await?;
    Ok(())
}

pub(crate) async fn patch(
    process: &Arc<Process>,
    context: &SessionContext,
    projection: &mut Projection,
    event: &ApplyPatchApprovalRequestEvent,
) -> protocol::Result<()> {
    let update = projection.tool(
        context,
        &event.call_id,
        "Apply patch".into(),
        ToolKind::Edit,
        ToolCallStatus::Pending,
        Some(serde_json::json!({"changes":event.changes,"reason":event.reason})),
    )?;
    // Never grant a persistent root even when the native request offers one.
    let decision = decision(context, update, true).await;
    submit(
        process,
        Op::PatchApproval {
            id: event.call_id.clone(),
            decision,
        },
        "native patch approval delivery failed",
    )
    .await?;
    Ok(())
}
