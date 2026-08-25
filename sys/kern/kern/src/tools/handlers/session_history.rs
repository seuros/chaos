use chaos_ipc::models::ContentItem;
use chaos_ipc::models::LocalShellAction;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::RolloutItem;
use chaos_journald::JournalEntry;
use chaos_journald::LoadedJournal;
use serde::Deserialize;
use serde::Serialize;

use crate::function_tool::FunctionCallError;
use crate::rollout::RolloutRecorder;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

use super::extract_function_arguments;
use super::parse_arguments;

const DEFAULT_ITEMS: usize = 40;
const MAX_ITEMS: usize = 100;
const DEFAULT_RESULTS: usize = 20;
const MAX_RESULTS: usize = 50;
const DEFAULT_BYTES: usize = 24_000;
const MIN_BYTES: usize = 1_000;
const MAX_BYTES: usize = 64_000;
const MAX_ENTRY_BYTES: usize = 8_000;
const MAX_EXCERPT_BYTES: usize = 2_400;
const TRUNCATION_MARKER: &str = "\n...[truncated]";

pub struct SessionHistoryHandler;

#[derive(Debug, Deserialize, Default)]
struct ReadSessionHistoryArgs {
    /// Exclusive journal sequence cursor. Defaults to the latest compaction,
    /// or the current journal end when the session has not compacted.
    #[serde(default)]
    before_seq: Option<i64>,
    #[serde(default)]
    max_items: Option<usize>,
    #[serde(default)]
    max_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct SearchSessionHistoryArgs {
    query: String,
    /// Exclusive journal sequence cursor. Defaults to the current journal end.
    #[serde(default)]
    before_seq: Option<i64>,
    #[serde(default)]
    max_results: Option<usize>,
    #[serde(default)]
    max_bytes: Option<usize>,
}

#[derive(Debug, Serialize)]
struct HistoryAnchor {
    kind: &'static str,
    seq: i64,
}

#[derive(Clone, Debug, Serialize)]
struct TranscriptEntry {
    seq: i64,
    recorded_at: String,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    text: String,
}

#[derive(Debug, Serialize)]
struct ReadSessionHistoryPayload {
    process_id: String,
    anchor: HistoryAnchor,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_compaction_seq: Option<i64>,
    entries: Vec<TranscriptEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_before_seq: Option<i64>,
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct SearchSessionHistoryPayload {
    process_id: String,
    query: String,
    searched_before_seq: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_compaction_seq: Option<i64>,
    matches: Vec<TranscriptEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_before_seq: Option<i64>,
    truncated: bool,
}

impl ToolHandler for SessionHistoryHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            tool_name,
            payload,
            ..
        } = invocation;
        let arguments = extract_function_arguments(payload, &tool_name)?;

        let recorder = {
            let guard = session.services.rollout.lock().await;
            guard.clone()
        }
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "this session has no persisted journal to inspect".to_string(),
            )
        })?;
        recorder.flush().await.map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to flush the session journal before reading it: {err}"
            ))
        })?;
        let journal = RolloutRecorder::get_journal_for_process(session.conversation_id)
            .await
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;

        let output = match tool_name.as_str() {
            "read_session_history" => {
                let args: ReadSessionHistoryArgs = parse_arguments(&arguments)?;
                let payload = read_session_history_payload(
                    session.conversation_id.to_string(),
                    &journal,
                    args,
                )?;
                serde_json::to_string_pretty(&payload)
            }
            "search_session_history" => {
                let args: SearchSessionHistoryArgs = parse_arguments(&arguments)?;
                let payload = search_session_history_payload(
                    session.conversation_id.to_string(),
                    &journal,
                    args,
                )?;
                serde_json::to_string_pretty(&payload)
            }
            _ => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "unsupported session history tool: {tool_name}"
                )));
            }
        }
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to serialize session history: {err}"))
        })?;

        Ok(FunctionToolOutput::from_text(output, Some(true)))
    }
}

fn read_session_history_payload(
    process_id: String,
    journal: &LoadedJournal,
    args: ReadSessionHistoryArgs,
) -> Result<ReadSessionHistoryPayload, FunctionCallError> {
    let latest_compaction_seq = latest_compaction_seq(&journal.items);
    let (anchor_kind, before_seq) = match args.before_seq {
        Some(before_seq) => (
            "explicit_cursor",
            validate_cursor(before_seq, journal.next_seq)?,
        ),
        None => latest_compaction_seq
            .map(|seq| ("latest_compaction", seq))
            .unwrap_or(("journal_end", journal.next_seq)),
    };
    let max_items = args.max_items.unwrap_or(DEFAULT_ITEMS).clamp(1, MAX_ITEMS);
    let max_bytes = args
        .max_bytes
        .unwrap_or(DEFAULT_BYTES)
        .clamp(MIN_BYTES, MAX_BYTES);

    let mut entries = Vec::new();
    let mut used_bytes = 0usize;
    let mut text_was_truncated = false;
    for entry in journal
        .items
        .iter()
        .rev()
        .filter(|entry| entry.seq < before_seq)
    {
        let Some(mut rendered) = render_journal_entry(entry) else {
            continue;
        };
        if entries.len() == max_items || used_bytes == max_bytes {
            break;
        }
        let allowance = MAX_ENTRY_BYTES.min(max_bytes - used_bytes);
        let (text, truncated) = truncate_to_bytes(&rendered.text, allowance);
        if text.is_empty() {
            break;
        }
        rendered.text = text;
        used_bytes = used_bytes.saturating_add(rendered.text.len());
        text_was_truncated |= truncated;
        entries.push(rendered);
    }
    entries.reverse();

    let next_before_seq = entries.first().and_then(|oldest| {
        journal
            .items
            .iter()
            .any(|entry| entry.seq < oldest.seq && render_journal_entry(entry).is_some())
            .then_some(oldest.seq)
    });
    let truncated = text_was_truncated || next_before_seq.is_some();

    Ok(ReadSessionHistoryPayload {
        process_id,
        anchor: HistoryAnchor {
            kind: anchor_kind,
            seq: before_seq,
        },
        latest_compaction_seq,
        entries,
        next_before_seq,
        truncated,
    })
}

fn search_session_history_payload(
    process_id: String,
    journal: &LoadedJournal,
    args: SearchSessionHistoryArgs,
) -> Result<SearchSessionHistoryPayload, FunctionCallError> {
    let query = args.query.trim();
    if query.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "query must not be empty".to_string(),
        ));
    }
    if query.len() > 500 {
        return Err(FunctionCallError::RespondToModel(
            "query must be at most 500 bytes".to_string(),
        ));
    }
    let before_seq = validate_cursor(
        args.before_seq.unwrap_or(journal.next_seq),
        journal.next_seq,
    )?;
    let max_results = args
        .max_results
        .unwrap_or(DEFAULT_RESULTS)
        .clamp(1, MAX_RESULTS);
    let max_bytes = args
        .max_bytes
        .unwrap_or(DEFAULT_BYTES)
        .clamp(MIN_BYTES, MAX_BYTES);

    let mut matches = Vec::new();
    let mut used_bytes = 0usize;
    let mut text_was_truncated = false;
    let mut more_matches = false;
    for mut entry in journal
        .items
        .iter()
        .rev()
        .filter(|entry| entry.seq < before_seq)
        .filter_map(render_journal_entry)
    {
        let Some(match_at) = find_literal_case_insensitive(&entry.text, query) else {
            continue;
        };
        if matches.len() == max_results || used_bytes == max_bytes {
            more_matches = true;
            break;
        }
        entry.text = excerpt_around(&entry.text, match_at, query.len(), MAX_EXCERPT_BYTES);
        let allowance = MAX_EXCERPT_BYTES.min(max_bytes - used_bytes);
        let (text, truncated) = truncate_to_bytes(&entry.text, allowance);
        if text.is_empty() {
            more_matches = true;
            break;
        }
        entry.text = text;
        used_bytes = used_bytes.saturating_add(entry.text.len());
        text_was_truncated |= truncated;
        matches.push(entry);
    }

    let next_before_seq = if more_matches {
        matches.last().map(|entry| entry.seq)
    } else {
        None
    };

    Ok(SearchSessionHistoryPayload {
        process_id,
        query: query.to_string(),
        searched_before_seq: before_seq,
        latest_compaction_seq: latest_compaction_seq(&journal.items),
        matches,
        next_before_seq,
        truncated: text_was_truncated || more_matches,
    })
}

fn validate_cursor(cursor: i64, next_seq: i64) -> Result<i64, FunctionCallError> {
    if !(0..=next_seq).contains(&cursor) {
        return Err(FunctionCallError::RespondToModel(format!(
            "before_seq must be between 0 and the journal end ({next_seq})"
        )));
    }
    Ok(cursor)
}

fn latest_compaction_seq(entries: &[JournalEntry]) -> Option<i64> {
    entries
        .iter()
        .rev()
        .find_map(|entry| matches!(entry.item, RolloutItem::Compacted(_)).then_some(entry.seq))
}

fn render_journal_entry(entry: &JournalEntry) -> Option<TranscriptEntry> {
    let (kind, role, text) = match &entry.item {
        RolloutItem::ResponseItem(item) => render_response_item(item)?,
        RolloutItem::Compacted(compacted) => (
            "compaction_summary",
            Some("assistant".to_string()),
            compacted.message.clone(),
        ),
        RolloutItem::SessionMeta(_) | RolloutItem::TurnContext(_) | RolloutItem::EventMsg(_) => {
            return None;
        }
    };
    (!text.trim().is_empty()).then(|| TranscriptEntry {
        seq: entry.seq,
        recorded_at: entry.recorded_at.to_string(),
        kind,
        role,
        text,
    })
}

fn render_response_item(item: &ResponseItem) -> Option<(&'static str, Option<String>, String)> {
    match item {
        ResponseItem::Message { role, content, .. } => Some((
            "message",
            Some(role.clone()),
            render_message_content(content),
        )),
        ResponseItem::FunctionCall {
            name,
            namespace,
            arguments,
            ..
        } => {
            let qualified_name = namespace
                .as_ref()
                .map(|namespace| format!("{namespace}.{name}"))
                .unwrap_or_else(|| name.clone());
            Some((
                "tool_call",
                Some("assistant".to_string()),
                format!("{qualified_name} {arguments}"),
            ))
        }
        ResponseItem::FunctionCallOutput {
            output, tool_name, ..
        }
        | ResponseItem::CustomToolCallOutput {
            output, tool_name, ..
        } => Some((
            "tool_result",
            Some("tool".to_string()),
            format!(
                "{}{}",
                tool_name
                    .as_deref()
                    .map(|name| format!("{name}: "))
                    .unwrap_or_default(),
                output
                    .body
                    .to_text()
                    .unwrap_or_else(|| "[image output omitted]".to_string())
            ),
        )),
        ResponseItem::CustomToolCall { name, input, .. } => Some((
            "tool_call",
            Some("assistant".to_string()),
            format!("{name} {input}"),
        )),
        ResponseItem::LocalShellCall { action, .. } => {
            let LocalShellAction::Exec(exec) = action;
            let cwd = exec
                .working_directory
                .as_deref()
                .map(|cwd| format!(" (cwd: {cwd})"))
                .unwrap_or_default();
            Some((
                "tool_call",
                Some("assistant".to_string()),
                format!("local_shell {}{cwd}", exec.command.join(" ")),
            ))
        }
        ResponseItem::ToolSearchCall {
            execution,
            arguments,
            ..
        } => Some((
            "tool_call",
            Some("assistant".to_string()),
            format!("tool_search[{execution}] {arguments}"),
        )),
        ResponseItem::ToolSearchOutput {
            status,
            execution,
            tools,
            ..
        } => Some((
            "tool_result",
            Some("tool".to_string()),
            format!(
                "tool_search[{execution}] status={status}, {} tool(s) returned",
                tools.len()
            ),
        )),
        ResponseItem::WebSearchCall { action, .. } => Some((
            "tool_call",
            Some("assistant".to_string()),
            format!(
                "web_search {}",
                action
                    .as_ref()
                    .and_then(|action| serde_json::to_string(action).ok())
                    .unwrap_or_else(|| "[details unavailable]".to_string())
            ),
        )),
        ResponseItem::ImageGenerationCall { revised_prompt, .. } => Some((
            "image_generation",
            Some("assistant".to_string()),
            revised_prompt
                .as_deref()
                .map(|prompt| format!("Generated image: {prompt}"))
                .unwrap_or_else(|| "Generated image [binary omitted]".to_string()),
        )),
        ResponseItem::Reasoning { .. }
        | ResponseItem::GhostSnapshot { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::CompactionTrigger {}
        | ResponseItem::Other => None,
    }
}

fn render_message_content(content: &[ContentItem]) -> String {
    content
        .iter()
        .map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => text.clone(),
            ContentItem::InputImage { .. } => "[image omitted]".to_string(),
            ContentItem::Document {
                name,
                mime_type,
                text,
            } => format!(
                "[document: {} ({mime_type})]\n{text}",
                name.as_deref().unwrap_or("unnamed")
            ),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn find_literal_case_insensitive(text: &str, query: &str) -> Option<usize> {
    if query.is_ascii() {
        text.as_bytes()
            .windows(query.len())
            .position(|window| window.eq_ignore_ascii_case(query.as_bytes()))
    } else {
        text.find(query)
    }
}

fn excerpt_around(text: &str, match_at: usize, match_len: usize, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let match_end = match_at.saturating_add(match_len).min(text.len());
    let mut start = match_at.saturating_sub(max_bytes / 3);
    start = previous_char_boundary(text, start);
    let mut end = next_char_boundary(text, start.saturating_add(max_bytes).min(text.len()));
    if end < match_end {
        end = next_char_boundary(text, match_end.min(text.len()));
        start = previous_char_boundary(text, end.saturating_sub(max_bytes));
    }
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if end < text.len() { "…" } else { "" };
    format!("{prefix}{}{suffix}", &text[start..end])
}

fn truncate_to_bytes(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    if max_bytes <= TRUNCATION_MARKER.len() {
        let end = previous_char_boundary(text, max_bytes);
        return (text[..end].to_string(), true);
    }
    let end = previous_char_boundary(text, max_bytes - TRUNCATION_MARKER.len());
    (format!("{}{TRUNCATION_MARKER}", &text[..end]), true)
}

fn previous_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn next_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
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
}
