use std::sync::Arc;

use crate::distill::InitialContextInjection;
use crate::distill::run_inline_auto_distill_task;
use crate::distill::should_use_remote_distill_task;
use crate::distill_remote::run_inline_remote_auto_distill_task;
use crate::error::ChaosErr;
use crate::error::Result as ChaosResult;
use tracing::warn;

use super::super::Session;
use super::super::TurnContext;

pub(super) async fn run_pre_sampling_compact(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
) -> ChaosResult<()> {
    if maybe_run_previous_model_inline_compact(sess, turn_context).await? {
        return Ok(());
    }
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
        match run_auto_compact(
            sess,
            &previous_model_turn_context,
            InitialContextInjection::DoNotInject,
        )
        .await
        {
            Ok(()) => return Ok(true),
            // The previous model may no longer be accepted for distillation
            // requests; fall back to the current model rather than failing
            // the turn.
            Err(ChaosErr::InvalidRequest(message)) => {
                warn!(
                    previous_model = %previous_model_turn_context.model_info.slug,
                    current_model = %turn_context.model_info.slug,
                    %message,
                    "previous-model distillation rejected; retrying with current model"
                );
                sess.services.session_telemetry.counter(
                    "chaos.distill.model_fallback",
                    /*inc*/ 1,
                    &[("reason", "invalid_request")],
                );
                run_auto_compact(sess, turn_context, InitialContextInjection::DoNotInject).await?;
                return Ok(true);
            }
            Err(err) => return Err(err),
        }
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
