use chaos_ipc::models::{DeveloperInstructions, ResponseItem};
use std::future::Future;
use tokio_util::sync::CancellationToken;

use crate::chaos::{Session, TurnContext};
use crate::error::{ChaosErr, Result as ChaosResult};

pub(super) async fn append(
    input: &mut Vec<ResponseItem>,
    session: &Session,
    turn: &TurnContext,
    cancellation: &CancellationToken,
) -> ChaosResult<()> {
    append_with_observation(
        input,
        turn.config.machine_warnings.enabled,
        cancellation,
        async {
            let observation = session.refresh_machine_recovery().await?;
            Ok(observation.warning_instruction)
        },
    )
    .await?;
    if turn.config.machine_warnings.enabled
        && let Some(instruction) = session.machine_recovery_instruction().await
    {
        input.push(DeveloperInstructions::new(instruction).into());
    }
    Ok(())
}

async fn append_with_observation(
    input: &mut Vec<ResponseItem>,
    enabled: bool,
    cancellation: &CancellationToken,
    observation: impl Future<Output = Result<Option<String>, String>>,
) -> ChaosResult<()> {
    if !enabled {
        return Ok(());
    }
    let observation = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(ChaosErr::TurnAborted),
        observation = observation => observation,
    };
    let message = match observation {
        Ok(instruction) => instruction,
        Err(error) => {
            tracing::warn!(%error, "machine warning check unavailable");
            Some(
                concat!(
                    "Machine check unavailable. Before interruption-sensitive work, ",
                    "ask the operator to verify power, storage, and cooling."
                )
                .to_string(),
            )
        }
    };
    if let Some(message) = message {
        input.push(DeveloperInstructions::new(message).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
