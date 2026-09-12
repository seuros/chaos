use chaos_ipc::models::{DeveloperInstructions, ResponseItem};
use tokio_util::sync::CancellationToken;

use crate::chaos::TurnContext;
use crate::error::{ChaosErr, Result as ChaosResult};
use crate::machine_status::ObservationRequest;

pub(super) async fn append(
    input: &mut Vec<ResponseItem>,
    turn: &TurnContext,
    cancellation: &CancellationToken,
) -> ChaosResult<()> {
    if !turn.config.machine_warnings.enabled {
        return Ok(());
    }
    let request = ObservationRequest::new(&turn.config, &turn.cwd);
    let observation = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(ChaosErr::TurnAborted),
        observation = request.observe() => observation,
    };
    let message = match observation {
        Ok(observation) => observation.warning_instruction,
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
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn machine_warnings_reach_the_request_as_ephemeral_developer_input() {
        let (_session, mut turn) = crate::chaos::make_session_and_context().await;
        let root = tempfile::tempdir().unwrap();
        turn.cwd = root.path().to_path_buf();
        Arc::make_mut(&mut turn.config)
            .machine_warnings
            .disk_free_percent = 100;
        // Native macOS host APIs can be slow under the shell test sandbox.
        Arc::make_mut(&mut turn.config)
            .machine_warnings
            .probe_timeout_ms = 60_000;
        let original: Vec<ResponseItem> = vec![];
        let mut request = original.clone();
        append(&mut request, &turn, &CancellationToken::new())
            .await
            .unwrap();
        assert!(original.is_empty(), "the journal input was not changed");
        let value = serde_json::to_value(&request).unwrap();
        // The internal ABI uses system; the OpenAI representer maps it to developer.
        assert_eq!(value[0]["role"], "system");
        let text = value[0]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Machine warning (harness host)"), "{text}");
        assert!(text.contains("Disk ("));
        assert!(text.contains("minimal checkpoint"));
        Arc::make_mut(&mut turn.config).machine_warnings.enabled = false;
        let mut next_request = original;
        append(&mut next_request, &turn, &CancellationToken::new())
            .await
            .unwrap();
        assert!(next_request.is_empty());
    }

    #[tokio::test]
    async fn machine_warnings_report_probe_timeout_without_inventing_measurements() {
        let (_session, mut turn) = crate::chaos::make_session_and_context().await;
        let _permit = crate::machine_status::lock_observations_for_test().await;
        Arc::make_mut(&mut turn.config)
            .machine_warnings
            .probe_timeout_ms = 1;
        let mut input = vec![];
        append(&mut input, &turn, &CancellationToken::new())
            .await
            .unwrap();
        let value = serde_json::to_value(&input).unwrap();
        let text = value[0]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Machine check unavailable"));
        assert!(text.contains("ask the operator"));
        assert!(text.contains("not safety clearance"));
        assert!(!text.contains("Discharging"));
        assert!(!text.contains("Disk ("));
    }

    #[tokio::test]
    async fn machine_warnings_respect_cancellation_without_starting_a_probe() {
        let (_session, turn) = crate::chaos::make_session_and_context().await;
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let mut input = vec![];
        assert!(matches!(
            append(&mut input, &turn, &cancellation).await,
            Err(ChaosErr::TurnAborted)
        ));
        assert!(input.is_empty());
    }
}
