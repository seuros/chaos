use std::sync::Arc;

use crate::distill::InitialContextInjection;
use crate::distill::run_inline_auto_distill_task;
use crate::distill::should_use_remote_distill_task;
use crate::distill_remote::run_inline_remote_auto_distill_task;
use crate::error::Result as ChaosResult;

use super::super::Session;
use super::super::TurnContext;

pub(super) async fn run_pre_sampling_compact(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
) -> ChaosResult<()> {
    maybe_run_previous_model_inline_compact(sess, turn_context).await?;
    if sess.allotment_status(turn_context).await.limit_reached {
        run_auto_compact(sess, turn_context, InitialContextInjection::DoNotInject).await?;
    }
    Ok(())
}

/// Runs pre-sampling compaction against the previous model when switching to a smaller
/// context-window model.
///
/// Returns `Ok(true)` when compaction ran successfully, `Ok(false)` when compaction was skipped
/// because the model/context-window preconditions were not met, and `Err(_)` only when compaction
/// was attempted and failed.
async fn maybe_run_previous_model_inline_compact(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
) -> ChaosResult<bool> {
    let Some(previous_turn_settings) = sess.previous_turn_settings().await else {
        return Ok(false);
    };
    let previous_model_turn_context = Arc::new(
        turn_context
            .with_model(previous_turn_settings.model, &sess.services.models_manager)
            .await,
    );

    let Some(old_context_window) = previous_model_turn_context.model_context_window() else {
        return Ok(false);
    };
    let Some(new_context_window) = turn_context.model_context_window() else {
        return Ok(false);
    };
    let should_run = sess.allotment_status(turn_context).await.limit_reached
        && previous_model_turn_context.model_info.slug != turn_context.model_info.slug
        && old_context_window > new_context_window;
    if should_run {
        run_auto_compact(
            sess,
            &previous_model_turn_context,
            InitialContextInjection::DoNotInject,
        )
        .await?;
        return Ok(true);
    }
    Ok(false)
}

pub(super) async fn run_auto_compact(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    initial_context_injection: InitialContextInjection,
) -> ChaosResult<()> {
    if should_use_remote_distill_task(&turn_context.provider) {
        run_inline_remote_auto_distill_task(
            Arc::clone(sess),
            Arc::clone(turn_context),
            initial_context_injection,
        )
        .await?;
    } else {
        run_inline_auto_distill_task(
            Arc::clone(sess),
            Arc::clone(turn_context),
            initial_context_injection,
        )
        .await?;
    }
    Ok(())
}
