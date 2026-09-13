use super::BEFORE_TURN_INPUT_FIXTURE;
use super::BEFORE_TURN_OUTPUT_FIXTURE;
use super::BeforeTurnCommandInput;
use super::SESSION_START_INPUT_FIXTURE;
use super::SESSION_START_OUTPUT_FIXTURE;
use super::STOP_INPUT_FIXTURE;
use super::STOP_OUTPUT_FIXTURE;
use super::SessionStartCommandInput;
use super::StopCommandInput;
use super::write_schema_fixtures;
use crate::HookAgentContext;
use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;

fn expected_fixture(name: &str) -> &'static str {
    match name {
        SESSION_START_INPUT_FIXTURE => {
            include_str!("../../schema/generated/session-start.command.input.schema.json")
        }
        SESSION_START_OUTPUT_FIXTURE => {
            include_str!("../../schema/generated/session-start.command.output.schema.json")
        }
        BEFORE_TURN_INPUT_FIXTURE => {
            include_str!("../../schema/generated/before-turn.command.input.schema.json")
        }
        BEFORE_TURN_OUTPUT_FIXTURE => {
            include_str!("../../schema/generated/before-turn.command.output.schema.json")
        }
        STOP_INPUT_FIXTURE => {
            include_str!("../../schema/generated/stop.command.input.schema.json")
        }
        STOP_OUTPUT_FIXTURE => {
            include_str!("../../schema/generated/stop.command.output.schema.json")
        }
        _ => panic!("unexpected fixture name: {name}"),
    }
}

fn normalize_newlines(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .trim_end_matches('\n')
        .to_string()
}

#[test]
fn generated_hook_schemas_match_fixtures() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let schema_root = temp_dir.path().join("schema");
    write_schema_fixtures(&schema_root).expect("write generated hook schemas");

    for fixture in [
        SESSION_START_INPUT_FIXTURE,
        SESSION_START_OUTPUT_FIXTURE,
        BEFORE_TURN_INPUT_FIXTURE,
        BEFORE_TURN_OUTPUT_FIXTURE,
        STOP_INPUT_FIXTURE,
        STOP_OUTPUT_FIXTURE,
    ] {
        let expected = normalize_newlines(expected_fixture(fixture));
        let actual = std::fs::read_to_string(schema_root.join("generated").join(fixture))
            .unwrap_or_else(|err| panic!("read generated schema {fixture}: {err}"));
        let actual = normalize_newlines(&actual);
        assert_eq!(expected, actual, "fixture should match generated schema");
    }
}

fn subagent_context() -> HookAgentContext {
    HookAgentContext {
        is_subagent: true,
        agent_id: Some("child-session".to_string()),
        agent_type: Some("scout".to_string()),
        parent_session_id: Some("parent-session".to_string()),
        agent_depth: Some(2),
    }
}

fn assert_subagent_context(value: &Value) {
    assert_eq!(value["is_subagent"], true);
    assert_eq!(value["agent_id"], "child-session");
    assert_eq!(value["agent_type"], "scout");
    assert_eq!(value["parent_session_id"], "parent-session");
    assert_eq!(value["agent_depth"], 2);
}

#[test]
fn session_start_input_serializes_structured_subagent_identity() {
    let value = serde_json::to_value(SessionStartCommandInput::new(
        "child-session",
        None,
        "/tmp/project",
        "test-model",
        "default",
        "startup",
        subagent_context(),
    ))
    .expect("serialize session start hook input");

    assert_subagent_context(&value);
}

#[test]
fn before_turn_input_serializes_structured_subagent_identity() {
    let value = serde_json::to_value(BeforeTurnCommandInput::new(
        "child-session",
        None,
        "/tmp/project",
        "turn-1",
        "test-model",
        "default",
        vec!["hello".to_string()],
        subagent_context(),
    ))
    .expect("serialize before turn hook input");

    assert_subagent_context(&value);
}

#[test]
fn stop_input_serializes_structured_subagent_identity() {
    let value = serde_json::to_value(StopCommandInput::new(
        "child-session",
        None,
        "/tmp/project",
        "test-model",
        "default",
        false,
        Some("done".to_string()),
        subagent_context(),
    ))
    .expect("serialize stop hook input");

    assert_subagent_context(&value);
}
