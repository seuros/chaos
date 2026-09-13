use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use std::borrow::Cow;
use std::path::Path;
use std::path::PathBuf;

use schemars::JsonSchema;
use schemars::Schema;
use schemars::SchemaGenerator;
use schemars::generate::SchemaSettings;

use crate::HookAgentContext;

const GENERATED_DIR: &str = "generated";
const BEFORE_TURN_INPUT_FIXTURE: &str = "before-turn.command.input.schema.json";
const BEFORE_TURN_OUTPUT_FIXTURE: &str = "before-turn.command.output.schema.json";
const SESSION_START_INPUT_FIXTURE: &str = "session-start.command.input.schema.json";
const SESSION_START_OUTPUT_FIXTURE: &str = "session-start.command.output.schema.json";
const STOP_INPUT_FIXTURE: &str = "stop.command.input.schema.json";
const STOP_OUTPUT_FIXTURE: &str = "stop.command.output.schema.json";

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub(crate) struct NullableString(Option<String>);

impl NullableString {
    fn from_path(path: Option<PathBuf>) -> Self {
        Self(path.map(|path| path.display().to_string()))
    }

    fn from_string(value: Option<String>) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct HookAgentCommandInput {
    pub is_subagent: bool,
    pub agent_id: NullableString,
    pub agent_type: NullableString,
    pub parent_session_id: NullableString,
    pub agent_depth: Option<i32>,
}

impl From<HookAgentContext> for HookAgentCommandInput {
    fn from(value: HookAgentContext) -> Self {
        Self {
            is_subagent: value.is_subagent,
            agent_id: NullableString::from_string(value.agent_id),
            agent_type: NullableString::from_string(value.agent_type),
            parent_session_id: NullableString::from_string(value.parent_session_id),
            agent_depth: value.agent_depth,
        }
    }
}

impl JsonSchema for NullableString {
    fn schema_name() -> Cow<'static, str> {
        "NullableString".into()
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        let mut schema = Schema::default();
        schema.insert(
            "type".to_string(),
            Value::Array(vec![
                Value::String("string".to_string()),
                Value::String("null".to_string()),
            ]),
        );
        schema
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct HookUniversalOutputWire {
    #[serde(default = "default_continue")]
    pub r#continue: bool,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub suppress_output: bool,
    #[serde(default)]
    pub system_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub(crate) enum HookEventNameWire {
    #[serde(rename = "SessionStart")]
    SessionStart,
    #[serde(rename = "BeforeTurn")]
    BeforeTurn,
    #[serde(rename = "Stop")]
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
#[schemars(rename = "session-start.command.output")]
pub(crate) struct SessionStartCommandOutputWire {
    #[serde(flatten)]
    pub universal: HookUniversalOutputWire,
    #[serde(default)]
    pub hook_specific_output: Option<SessionStartHookSpecificOutputWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionStartHookSpecificOutputWire {
    pub hook_event_name: HookEventNameWire,
    #[serde(default)]
    pub additional_context: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
#[schemars(rename = "before-turn.command.output")]
pub(crate) struct BeforeTurnCommandOutputWire {
    #[serde(flatten)]
    pub universal: HookUniversalOutputWire,
    #[serde(default)]
    pub hook_specific_output: Option<BeforeTurnHookSpecificOutputWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct BeforeTurnHookSpecificOutputWire {
    pub hook_event_name: HookEventNameWire,
    #[serde(default)]
    pub additional_context: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
#[schemars(rename = "stop.command.output")]
pub(crate) struct StopCommandOutputWire {
    #[serde(flatten)]
    pub universal: HookUniversalOutputWire,
    #[serde(default)]
    pub decision: Option<StopDecisionWire>,
    /// Claude requires `reason` when `decision` is `block`; we enforce that
    /// semantic rule during output parsing rather than in the JSON schema.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub(crate) enum StopDecisionWire {
    #[serde(rename = "block")]
    Block,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "session-start.command.input")]
pub(crate) struct SessionStartCommandInput {
    pub session_id: String,
    pub transcript_path: NullableString,
    pub cwd: String,
    #[schemars(schema_with = "session_start_hook_event_name_schema")]
    pub hook_event_name: String,
    pub model: String,
    #[schemars(schema_with = "permission_mode_schema")]
    pub permission_mode: String,
    #[schemars(schema_with = "session_start_source_schema")]
    pub source: String,
    #[serde(flatten)]
    pub agent_context: HookAgentCommandInput,
}

impl SessionStartCommandInput {
    pub(crate) fn new(
        session_id: impl Into<String>,
        transcript_path: Option<PathBuf>,
        cwd: impl Into<String>,
        model: impl Into<String>,
        permission_mode: impl Into<String>,
        source: impl Into<String>,
        agent_context: HookAgentContext,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            transcript_path: NullableString::from_path(transcript_path),
            cwd: cwd.into(),
            hook_event_name: "SessionStart".to_string(),
            model: model.into(),
            permission_mode: permission_mode.into(),
            source: source.into(),
            agent_context: agent_context.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "before-turn.command.input")]
pub(crate) struct BeforeTurnCommandInput {
    pub session_id: String,
    pub transcript_path: NullableString,
    pub cwd: String,
    #[schemars(schema_with = "before_turn_hook_event_name_schema")]
    pub hook_event_name: String,
    pub turn_id: String,
    pub model: String,
    #[schemars(schema_with = "permission_mode_schema")]
    pub permission_mode: String,
    pub input_messages: Vec<String>,
    #[serde(flatten)]
    pub agent_context: HookAgentCommandInput,
}

impl BeforeTurnCommandInput {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        session_id: impl Into<String>,
        transcript_path: Option<PathBuf>,
        cwd: impl Into<String>,
        turn_id: impl Into<String>,
        model: impl Into<String>,
        permission_mode: impl Into<String>,
        input_messages: Vec<String>,
        agent_context: HookAgentContext,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            transcript_path: NullableString::from_path(transcript_path),
            cwd: cwd.into(),
            hook_event_name: "BeforeTurn".to_string(),
            turn_id: turn_id.into(),
            model: model.into(),
            permission_mode: permission_mode.into(),
            input_messages,
            agent_context: agent_context.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "stop.command.input")]
pub(crate) struct StopCommandInput {
    pub session_id: String,
    pub transcript_path: NullableString,
    pub cwd: String,
    #[schemars(schema_with = "stop_hook_event_name_schema")]
    pub hook_event_name: String,
    pub model: String,
    #[schemars(schema_with = "permission_mode_schema")]
    pub permission_mode: String,
    pub stop_hook_active: bool,
    pub last_assistant_message: NullableString,
    #[serde(flatten)]
    pub agent_context: HookAgentCommandInput,
}

impl StopCommandInput {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        session_id: impl Into<String>,
        transcript_path: Option<PathBuf>,
        cwd: impl Into<String>,
        model: impl Into<String>,
        permission_mode: impl Into<String>,
        stop_hook_active: bool,
        last_assistant_message: Option<String>,
        agent_context: HookAgentContext,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            transcript_path: NullableString::from_path(transcript_path),
            cwd: cwd.into(),
            hook_event_name: "Stop".to_string(),
            model: model.into(),
            permission_mode: permission_mode.into(),
            stop_hook_active,
            last_assistant_message: NullableString::from_string(last_assistant_message),
            agent_context: agent_context.into(),
        }
    }
}

pub fn write_schema_fixtures(schema_root: &Path) -> anyhow::Result<()> {
    let generated_dir = schema_root.join(GENERATED_DIR);
    ensure_empty_dir(&generated_dir)?;

    write_schema(
        &generated_dir.join(BEFORE_TURN_INPUT_FIXTURE),
        schema_json::<BeforeTurnCommandInput>()?,
    )?;
    write_schema(
        &generated_dir.join(BEFORE_TURN_OUTPUT_FIXTURE),
        schema_json::<BeforeTurnCommandOutputWire>()?,
    )?;
    write_schema(
        &generated_dir.join(SESSION_START_INPUT_FIXTURE),
        schema_json::<SessionStartCommandInput>()?,
    )?;
    write_schema(
        &generated_dir.join(SESSION_START_OUTPUT_FIXTURE),
        schema_json::<SessionStartCommandOutputWire>()?,
    )?;
    write_schema(
        &generated_dir.join(STOP_INPUT_FIXTURE),
        schema_json::<StopCommandInput>()?,
    )?;
    write_schema(
        &generated_dir.join(STOP_OUTPUT_FIXTURE),
        schema_json::<StopCommandOutputWire>()?,
    )?;

    Ok(())
}

fn write_schema(path: &Path, json: Vec<u8>) -> anyhow::Result<()> {
    std::fs::write(path, json)?;
    Ok(())
}

fn ensure_empty_dir(dir: &Path) -> anyhow::Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    Ok(())
}

fn schema_json<T>() -> anyhow::Result<Vec<u8>>
where
    T: JsonSchema,
{
    let schema = schema_for_type::<T>();
    let value = serde_json::to_value(schema)?;
    let value = canonicalize_json(&value);
    let mut json = serde_json::to_vec_pretty(&value)?;
    json.push(b'\n');
    Ok(json)
}

fn schema_for_type<T>() -> Schema
where
    T: JsonSchema,
{
    SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<T>()
}

fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_json).collect()),
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by_key(|(left, _)| *left);
            let mut sorted = Map::with_capacity(map.len());
            for (key, child) in entries {
                sorted.insert(key.clone(), canonicalize_object_child(key, child));
            }
            Value::Object(sorted)
        }
        _ => value.clone(),
    }
}

fn canonicalize_object_child(key: &str, value: &Value) -> Value {
    let value = canonicalize_json(value);
    if key == "required" {
        return sort_string_array(value);
    }
    value
}

fn sort_string_array(value: Value) -> Value {
    match value {
        Value::Array(items) => {
            let mut strings = Vec::with_capacity(items.len());
            for item in &items {
                let Some(value) = item.as_str() else {
                    return Value::Array(items);
                };
                strings.push(value.to_string());
            }
            strings.sort();
            Value::Array(strings.into_iter().map(Value::String).collect())
        }
        other => other,
    }
}

fn session_start_hook_event_name_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_const_schema("SessionStart")
}

fn before_turn_hook_event_name_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_const_schema("BeforeTurn")
}

fn stop_hook_event_name_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_const_schema("Stop")
}

fn permission_mode_schema(_gen: &mut SchemaGenerator) -> Schema {
    // There are only two execution modes: interactive (`default`) and
    // headless (`bypassPermissions`). `acceptEdits`, `plan`, and `dontAsk`
    // are operator profiles layered on interactive mode, not separate flows,
    // so they have no business being advertised to hook authors.
    string_enum_schema(&["default", "bypassPermissions"])
}

fn session_start_source_schema(_gen: &mut SchemaGenerator) -> Schema {
    string_enum_schema(&["startup", "resume", "clear"])
}

fn string_const_schema(value: &str) -> Schema {
    let mut schema = Schema::default();
    schema.insert("type".to_string(), Value::String("string".to_string()));
    schema.insert("const".to_string(), Value::String(value.to_string()));
    schema
}

fn string_enum_schema(values: &[&str]) -> Schema {
    let mut schema = Schema::default();
    schema.insert("type".to_string(), Value::String("string".to_string()));
    schema.insert(
        "enum".to_string(),
        Value::Array(
            values
                .iter()
                .map(|value| Value::String((*value).to_string()))
                .collect(),
        ),
    );
    schema
}

fn default_continue() -> bool {
    true
}

#[cfg(test)]
mod tests;
