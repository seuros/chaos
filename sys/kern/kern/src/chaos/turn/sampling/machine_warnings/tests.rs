use super::*;
use crate::machine_warnings::{MachineWarning, instructions};
use chaos_machine::{FilesystemId, StorageRole, StorageTarget};
use std::cell::Cell;
use std::future::{pending, ready};

#[tokio::test]
async fn machine_warnings_reach_the_request_as_ephemeral_developer_input() {
    let warning = instructions(&[MachineWarning::LowDiskSpace {
        filesystem: FilesystemId {
            device: 1,
            filesystem: 1,
        },
        targets: vec![StorageTarget {
            role: StorageRole::Workspace,
            path: "/workspace".into(),
        }],
        available_bytes: 1,
        available_percent: 1.0,
    }])
    .expect("low disk warning");
    let original: Vec<ResponseItem> = vec![DeveloperInstructions::new("persistent context").into()];

    for _ in 0..2 {
        let mut request = original.clone();
        append_with_observation(
            &mut request,
            true,
            &CancellationToken::new(),
            ready(Ok(Some(warning.clone()))),
        )
        .await
        .unwrap();
        assert_eq!(request.len(), original.len() + 1);
        assert_eq!(&request[..original.len()], original.as_slice());
        let value = serde_json::to_value(&request).unwrap();
        // The internal ABI uses system; the OpenAI representer maps it to developer.
        assert_eq!(value[1]["role"], "system");
        let text = value[1]["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, warning);
        assert!(text.contains("Machine warning (harness host)"));
        assert!(text.contains("Disk ("));
        assert!(text.contains("minimal checkpoint"));
        assert!(text.contains("Tell the operator"));
    }
    assert_eq!(original.len(), 1, "the journal input was not changed");
}

#[tokio::test]
async fn machine_warnings_do_not_start_a_probe_when_disabled() {
    let original = vec![DeveloperInstructions::new("persistent context").into()];
    let mut request = original.clone();
    append_with_observation(&mut request, false, &CancellationToken::new(), async {
        panic!("disabled warnings must not poll the observation");
    })
    .await
    .unwrap();
    assert_eq!(request, original);
}

#[tokio::test]
async fn machine_warnings_leave_input_unchanged_without_warnings() {
    let original = vec![DeveloperInstructions::new("persistent context").into()];
    let mut request = original.clone();
    append_with_observation(
        &mut request,
        true,
        &CancellationToken::new(),
        ready(Ok(None)),
    )
    .await
    .unwrap();
    assert_eq!(request, original);
}

#[tokio::test]
async fn machine_warnings_report_probe_failure_without_inventing_measurements() {
    let mut input = vec![];
    append_with_observation(
        &mut input,
        true,
        &CancellationToken::new(),
        ready(Err(
            "machine observations unavailable: probe timed out".into()
        )),
    )
    .await
    .unwrap();
    assert_eq!(input.len(), 1);
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
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut input = vec![];
    assert!(matches!(
        append_with_observation(&mut input, true, &cancellation, async {
            panic!("cancelled warnings must not poll the observation");
        })
        .await,
        Err(ChaosErr::TurnAborted)
    ));
    assert!(input.is_empty());
}

#[tokio::test]
async fn machine_warnings_cancel_a_pending_observation_without_changing_input() {
    let cancellation = CancellationToken::new();
    let polled = Cell::new(false);
    let original = vec![DeveloperInstructions::new("persistent context").into()];
    let mut input = original.clone();
    {
        let observation = async {
            polled.set(true);
            pending().await
        };
        let mut append = std::pin::pin!(append_with_observation(
            &mut input,
            true,
            &cancellation,
            observation,
        ));
        assert!(futures::poll!(&mut append).is_pending());
        assert!(polled.get());
        cancellation.cancel();
        assert!(matches!(
            futures::poll!(&mut append),
            std::task::Poll::Ready(Err(ChaosErr::TurnAborted))
        ));
    }
    assert_eq!(input, original);
}
