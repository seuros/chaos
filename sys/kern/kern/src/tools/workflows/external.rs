//! Dynamic kernel gate around the SDK's negotiated, task-routed requester.
//! Never use the native provider or picker for an external client's interaction.

use super::dispatch::Caller;
use crate::tools::parallel::ToolExecutionLease;
use mcp_host::bt::{InteractionFuture, TreeInteractionContext, TreeInteractions};
use mcp_host::prelude::ClientRequester;
use mcp_host::protocol::elicitation::ElicitationSchema;
use mcp_host::protocol::types::{
    CreateElicitationResult, CreateMessageParams, CreateMessageResult,
};
use std::time::Duration;

pub(super) struct ExternalInteractions {
    pub(super) caller: Caller,
    pub(super) requester: Option<ClientRequester>,
}

impl ExternalInteractions {
    async fn authorize(&self) -> Result<ToolExecutionLease, String> {
        let session = &self.caller.invocation.session;
        let cancellation = &self.caller.cancellation;
        let permit = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err("workflow interaction cancelled".into()),
            permit = session.tool_scheduler().acquire(false) =>
                permit.map_err(|error| error.to_string())?,
        };
        let turn = session
            .try_effective_turn_context(&self.caller.invocation.turn)
            .await
            .map_err(|error| error.to_string())?;
        let router = crate::chaos::built_tools(session, &turn, &[], cancellation)
            .await
            .map_err(|error| error.to_string())?;
        if !router
            .visible_specs()
            .iter()
            .any(|spec| spec.name() == "run_workflow")
        {
            return Err("workflows are no longer enabled for this caller".into());
        }
        let state = session.state.lock().await;
        if state.machine_recovery.parked() || state.machine_recovery.requested_turn().is_some() {
            return Err("machine recovery prevents workflow interaction".into());
        }
        Ok(permit)
    }
}

impl TreeInteractions for ExternalInteractions {
    fn ask<'a>(
        &'a self,
        _: TreeInteractionContext<'a>,
        message: String,
        schema: &'a ElicitationSchema,
    ) -> InteractionFuture<'a, CreateElicitationResult> {
        Box::pin(async move {
            let _permit = self.authorize().await?;
            self.requester
                .as_ref()
                .ok_or("no negotiated client requester")?
                .request_elicitation_form(message, schema, Some(Duration::MAX))
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn sample<'a>(
        &'a self,
        _: TreeInteractionContext<'a>,
        params: CreateMessageParams,
    ) -> InteractionFuture<'a, CreateMessageResult> {
        Box::pin(async move {
            let _permit = self.authorize().await?;
            self.requester
                .as_ref()
                .ok_or("no negotiated client requester")?
                .request_sampling(params, Some(Duration::MAX))
                .await
                .map_err(|error| error.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_synopsis::runs::{RunKind, RunRegistry};
    use rama::futures::FutureExt;
    use serde_json::json;

    #[tokio::test]
    async fn queued_interaction_rechecks_group_and_cancel_before_client_dispatch()
    -> anyhow::Result<()> {
        for cancel in [false, true] {
            let (session, turn, tracker) = super::super::tests::fixture().await;
            session.services.tool_group_catalog.set_groups_enabled(
                &session.services.tool_group_state,
                [crate::tools::groups::WORKFLOWS],
                true,
            )?;
            let registry = RunRegistry::default();
            let lease = registry.start(RunKind::Workflow)?;
            let caller = Caller {
                invocation: std::sync::Arc::new(super::super::tests::invocation(
                    session.clone(),
                    turn,
                    tracker,
                    "run_workflow",
                    json!({}),
                )),
                cancellation: lease.cancellation(),
                run_id: lease.id(),
                tasks: tokio_util::task::TaskTracker::new(),
            };
            let interactions = ExternalInteractions {
                caller,
                requester: None,
            };
            let held = session.services.tool_scheduler.acquire(false).await?;
            let mut authorize = Box::pin(interactions.authorize());
            assert!(authorize.as_mut().now_or_never().is_none());
            if cancel {
                interactions.caller.cancellation.cancel();
            } else {
                session.services.tool_group_catalog.set_groups_enabled(
                    &session.services.tool_group_state,
                    [crate::tools::groups::WORKFLOWS],
                    false,
                )?;
            }
            drop(held);
            let error = tokio::time::timeout(Duration::from_secs(5), authorize)
                .await?
                .err()
                .ok_or_else(|| anyhow::anyhow!("authorization unexpectedly succeeded"))?;
            assert!(
                error.contains(if cancel {
                    "cancelled"
                } else {
                    "no longer enabled"
                }),
                "{error}"
            );
        }
        Ok(())
    }
}
