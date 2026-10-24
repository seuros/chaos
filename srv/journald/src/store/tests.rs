use super::*;
use chaos_ipc::models::ResponseItem;

fn entry(seq: i64) -> JournalEntry {
    JournalEntry {
        seq,
        recorded_at: jiff::Timestamp::UNIX_EPOCH,
        item: RolloutItem::ResponseItem(ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: Vec::new(),
            end_turn: None,
            phase: None,
        }),
    }
}

#[test]
fn sequence_validation_handles_initial_and_appended_batches() {
    let process_id = ProcessId::new();
    for first_seq in [0, 9] {
        assert!(validate_sequences(process_id, first_seq, &[]).is_ok());
        assert!(
            validate_sequences(
                process_id,
                first_seq,
                &[entry(first_seq), entry(first_seq + 1)]
            )
            .is_ok()
        );
        match validate_sequences(
            process_id,
            first_seq,
            &[entry(first_seq), entry(first_seq + 2)],
        ) {
            Err(JournalError::SequenceConflict {
                process_id: actual_process,
                expected_next_seq,
                actual_next_seq,
            }) => {
                assert_eq!(actual_process, process_id);
                assert_eq!(expected_next_seq, first_seq + 1);
                assert_eq!(actual_next_seq, first_seq + 2);
            }
            other => panic!("expected the first sequence mismatch, got {other:?}"),
        }
    }
}

#[test]
fn lease_timestamps_preserve_second_precision_and_reject_overflow() {
    let base = jiff::Timestamp::from_second(42).expect("valid timestamp");
    assert_eq!(
        timestamp_after(base, Duration::from_millis(1999))
            .expect("valid expiration")
            .as_second(),
        43
    );
    assert!(timestamp_after(base, Duration::from_secs(u64::MAX)).is_err());
    assert!(timestamp_from_epoch_seconds(i64::MAX).is_err());
}
