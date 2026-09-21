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
            let mut instruction = observation.warning_instruction;
            if session.machine_recovery_status().await.phase == crate::machine_recovery::Phase::Recovered {
                // Only fixed vocabulary and measured values enter privileged text.
                let temperatures = observation.machine.thermal.cpu_temperatures.iter()
                    .filter_map(|sensor| sensor.celsius.filter(|value| value.is_finite())
                        .map(|value| (sensor.kind, value)))
                    .collect::<Vec<_>>();
                let disk = observation.storage.filesystems.iter()
                    .filter_map(chaos_machine::Filesystem::available_percent)
                    .collect::<Vec<_>>();
                let measurements = format!(
                    "Current recovery observations (harness host): external power {:?}; CPU channels (kind, degrees) {:?}; relevant filesystem free percentages {:?}. These readings are not a guarantee the previous workload is sustainable.",
                    observation.machine.power.external_power, temperatures, disk,
                );
                instruction = Some(match instruction {
                    Some(warning) => format!("{warning}\n{measurements}"),
                    None => measurements,
                });
            }
            Ok(instruction)
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
