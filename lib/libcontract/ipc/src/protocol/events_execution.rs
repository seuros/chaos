use std::path::PathBuf;
use std::time::Duration;

use crate::parse_command::ParsedCommand;
use base64::Engine as _;
use base64::alphabet;
use base64::engine::DecodePaddingMode;
use base64::engine::GeneralPurpose;
use base64::engine::GeneralPurposeConfig;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use strum_macros::Display;
use ts_rs::TS;

#[derive(
    Debug, Clone, Copy, Display, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandSource {
    #[default]
    Agent,
    UserShell,
    UnifiedExecStartup,
    UnifiedExecInteraction,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandStatus {
    Completed,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ExecCommandBeginEvent {
    /// Identifier so this can be paired with the ExecCommandEnd event.
    pub call_id: String,
    /// Identifier for the underlying PTY process (when available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub process_id: Option<String>,
    /// Turn ID that this command belongs to.
    pub turn_id: String,
    /// The command to be executed.
    pub command: Vec<String>,
    /// The command's working directory if not the default cwd for the agent.
    pub cwd: PathBuf,
    pub parsed_cmd: Vec<ParsedCommand>,
    /// Where the command originated. Defaults to Agent for backward compatibility.
    #[serde(default)]
    pub source: ExecCommandSource,
    /// Raw input sent to a unified exec session (if this is an interaction event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub interaction_input: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ExecCommandEndEvent {
    /// Identifier for the ExecCommandBegin that finished.
    pub call_id: String,
    /// Identifier for the underlying PTY process (when available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub process_id: Option<String>,
    /// Turn ID that this command belongs to.
    pub turn_id: String,
    /// The command that was executed.
    pub command: Vec<String>,
    /// The command's working directory if not the default cwd for the agent.
    pub cwd: PathBuf,
    pub parsed_cmd: Vec<ParsedCommand>,
    /// Where the command originated. Defaults to Agent for backward compatibility.
    #[serde(default)]
    pub source: ExecCommandSource,
    /// Raw input sent to a unified exec session (if this is an interaction event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub interaction_input: Option<String>,

    /// Captured stdout
    pub stdout: String,
    /// Captured stderr
    pub stderr: String,
    /// Captured aggregated output
    #[serde(default)]
    pub aggregated_output: String,
    /// The command's exit code.
    pub exit_code: i32,
    /// The duration of the command execution.
    #[ts(type = "string")]
    pub duration: Duration,
    /// Formatted output from the command, as seen by the model.
    pub formatted_output: String,
    /// Completion status for this command execution.
    pub status: ExecCommandStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ExecCommandOutputDeltaEvent {
    /// Identifier for the ExecCommandBegin that produced this chunk.
    pub call_id: String,
    /// Which stream produced this chunk.
    pub stream: ExecOutputStream,
    /// Raw bytes from the stream (may not be valid UTF-8).
    #[serde(with = "base64_bytes")]
    #[schemars(with = "String")]
    #[ts(type = "string")]
    pub chunk: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct TerminalInteractionEvent {
    /// Identifier for the ExecCommandBegin that produced this chunk.
    pub call_id: String,
    /// Process id associated with the running command.
    pub process_id: String,
    /// Stdin sent to the running session.
    pub stdin: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ViewImageToolCallEvent {
    /// Identifier for the originating tool call.
    pub call_id: String,
    /// Local filesystem path provided to the tool.
    pub path: PathBuf,
}

mod base64_bytes {
    use serde::Deserializer;
    use serde::Serializer;
    use serde::de::Error as _;

    use super::*;

    fn engine() -> GeneralPurpose {
        GeneralPurpose::new(
            &alphabet::STANDARD,
            GeneralPurposeConfig::new()
                .with_encode_padding(true)
                .with_decode_padding_mode(DecodePaddingMode::Indifferent),
        )
    }

    pub(super) fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&engine().encode(bytes))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        engine().decode(encoded).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_delta_uses_padded_base64_and_accepts_unpadded_input() {
        let event = ExecCommandOutputDeltaEvent {
            call_id: "call-1".to_owned(),
            stream: ExecOutputStream::Stdout,
            chunk: b"Hello World".to_vec(),
        };

        let mut value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["chunk"], "SGVsbG8gV29ybGQ=");

        value["chunk"] = "SGVsbG8gV29ybGQ".into();
        let decoded: ExecCommandOutputDeltaEvent = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, event);
    }
}
