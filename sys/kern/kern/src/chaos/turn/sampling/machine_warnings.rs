use chaos_ipc::models::{DeveloperInstructions, ResponseItem};
use std::future::Future;
use tokio_util::sync::CancellationToken;

use crate::chaos::TurnContext;
use crate::error::{ChaosErr, Result as ChaosResult};
use crate::machine_status::ObservationRequest;

pub(super) async fn append(
    input: &mut Vec<ResponseItem>,
    turn: &TurnContext,
    cancellation: &CancellationToken,
) -> ChaosResult<()> {
    append_with_observation(
        input,
        turn.config.machine_warnings.enabled,
        cancellation,
        async {
            ObservationRequest::new(&turn.config, &turn.cwd)
                .observe()
                .await
                .map(|observation| observation.warning_instruction)
        },
    )
    .await
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
            Some("Machine check unavailable. Before interruption-sensitive work, ask the operator to verify power, storage and cooling; missing observations are not safety clearance.".to_string())
        }
    };
    if let Some(message) = message {
        input.push(DeveloperInstructions::new(message).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
