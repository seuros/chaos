use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::CompactedItem;
use chaos_ipc::protocol::RolloutItem;
use chaos_journald::JournalEntry;
use chaos_journald::LoadedJournal;
use jiff::Timestamp;

use super::ReadSessionHistoryArgs;
use super::SearchSessionHistoryArgs;
use super::read_session_history_payload;
use super::search_session_history_payload;

fn message(seq: i64, role: &str, text: &str) -> JournalEntry {
    JournalEntry {
        seq,
        recorded_at: Timestamp::from_second(seq).expect("valid timestamp"),
        item: RolloutItem::ResponseItem(ResponseItem::Message {
            id: None,
            role: role.to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase: None,
        }),
    }
}

fn compacted(seq: i64, summary: &str) -> JournalEntry {
    JournalEntry {
        seq,
        recorded_at: Timestamp::from_second(seq).expect("valid timestamp"),
        item: RolloutItem::Compacted(CompactedItem {
            message: summary.to_string(),
            replacement_history: Some(Vec::new()),
        }),
    }
}

fn journal(items: Vec<JournalEntry>) -> LoadedJournal {
    let next_seq = items.last().map(|entry| entry.seq + 1).unwrap_or(0);
    LoadedJournal {
        process_id: chaos_ipc::ProcessId::default(),
        parent: None,
        items,
        next_seq,
    }
}

#[test]
fn read_defaults_to_entries_before_latest_compaction() {
    let journal = journal(vec![
        message(0, "user", "first room"),
        message(1, "assistant", "still here"),
        compacted(2, "summary"),
        message(3, "user", "new room"),
    ]);

    let payload = read_session_history_payload(
        "process".to_string(),
        &journal,
        ReadSessionHistoryArgs::default(),
    )
    .expect("read history");

    assert_eq!(payload.anchor.kind, "latest_compaction");
    assert_eq!(payload.anchor.seq, 2);
    assert_eq!(payload.latest_compaction_seq, Some(2));
    assert_eq!(
        payload
            .entries
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>(),
        vec!["first room", "still here"]
    );
}

#[test]
fn read_pages_backward_with_sequence_cursor() {
    let journal = journal(vec![
        message(0, "user", "zero"),
        message(1, "assistant", "one"),
        message(2, "user", "two"),
    ]);

    let payload = read_session_history_payload(
        "process".to_string(),
        &journal,
        ReadSessionHistoryArgs {
            before_seq: None,
            max_items: Some(2),
            max_bytes: None,
        },
    )
    .expect("read history");

    assert_eq!(
        payload
            .entries
            .iter()
            .map(|entry| entry.seq)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(payload.next_before_seq, Some(1));
    assert!(payload.truncated);
}

#[test]
fn read_applies_a_hard_text_budget() {
    let journal = journal(vec![message(0, "user", &"a".repeat(2_000))]);

    let payload = read_session_history_payload(
        "process".to_string(),
        &journal,
        ReadSessionHistoryArgs {
            before_seq: None,
            max_items: None,
            max_bytes: Some(1_000),
        },
    )
    .expect("read history");

    assert_eq!(payload.entries.len(), 1);
    assert!(payload.entries[0].text.len() <= 1_000);
    assert!(payload.entries[0].text.ends_with("...[truncated]"));
    assert!(payload.truncated);
}

#[test]
fn search_is_case_insensitive_and_excludes_reasoning() {
    let mut items = vec![
        message(0, "user", "The blue doorway"),
        message(1, "assistant", "A BLUE answer"),
    ];
    items.push(JournalEntry {
        seq: 2,
        recorded_at: Timestamp::from_second(2).expect("valid timestamp"),
        item: RolloutItem::ResponseItem(ResponseItem::Reasoning {
            id: String::new(),
            summary: Vec::new(),
            content: None,
            encrypted_content: Some("blue hidden reasoning".to_string()),
        }),
    });
    let journal = journal(items);

    let payload = search_session_history_payload(
        "process".to_string(),
        &journal,
        SearchSessionHistoryArgs {
            query: "blue".to_string(),
            before_seq: None,
            max_results: None,
            max_bytes: None,
        },
    )
    .expect("search history");

    assert_eq!(
        payload
            .matches
            .iter()
            .map(|entry| entry.seq)
            .collect::<Vec<_>>(),
        vec![1, 0]
    );
}

#[test]
fn search_cursor_pages_older_matches() {
    let journal = journal(vec![
        message(0, "user", "needle zero"),
        message(1, "assistant", "needle one"),
        message(2, "user", "needle two"),
    ]);

    let first = search_session_history_payload(
        "process".to_string(),
        &journal,
        SearchSessionHistoryArgs {
            query: "needle".to_string(),
            before_seq: None,
            max_results: Some(2),
            max_bytes: None,
        },
    )
    .expect("first search page");
    assert_eq!(first.next_before_seq, Some(1));

    let second = search_session_history_payload(
        "process".to_string(),
        &journal,
        SearchSessionHistoryArgs {
            query: "needle".to_string(),
            before_seq: first.next_before_seq,
            max_results: Some(2),
            max_bytes: None,
        },
    )
    .expect("second search page");
    assert_eq!(
        second
            .matches
            .iter()
            .map(|entry| entry.seq)
            .collect::<Vec<_>>(),
        vec![0]
    );
}
