use crate::exec::ExecToolCallOutput;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum ExecError {
    #[error("Failed to create exec process: {message}")]
    CreateProcess { message: String },
    // The model is trained on `session_id`, but internally we track a `process_id`.
    #[error("Unknown process id {process_id}")]
    UnknownProcessId { process_id: i32 },
    #[error("failed to write to stdin")]
    WriteToStdin,
    #[error(
        "stdin is closed for this session; rerun exec_command with tty=true to keep stdin open"
    )]
    StdinClosed,
    #[error("missing command line for exec request")]
    MissingCommandLine,
    #[error("Command denied by sandbox: {message}")]
    SandboxDenied {
        message: String,
        output: ExecToolCallOutput,
    },
}

impl ExecError {
    pub(super) fn from_tool_error(error: crate::tools::sandboxing::ToolError) -> Self {
        use crate::error::{ChaosErr, SandboxErr, get_error_message_ui};
        use crate::tools::sandboxing::ToolError;

        let message = match &error {
            ToolError::Chaos(
                error @ ChaosErr::Sandbox(SandboxErr::Denied {
                    network_policy_decision: None,
                    ..
                }),
            ) => get_error_message_ui(error),
            other => format!("{other:?}"),
        };
        match error {
            ToolError::Chaos(ChaosErr::Sandbox(SandboxErr::Denied { output, .. })) => {
                Self::sandbox_denied(message, *output)
            }
            _ => Self::create_process(message),
        }
    }

    pub(crate) fn create_process(message: String) -> Self {
        Self::CreateProcess { message }
    }

    pub(crate) fn sandbox_denied(message: String, output: ExecToolCallOutput) -> Self {
        Self::SandboxDenied { message, output }
    }
}

#[cfg(test)]
mod tests;
