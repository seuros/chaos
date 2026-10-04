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
    pub(crate) fn create_process(message: String) -> Self {
        Self::CreateProcess { message }
    }

    pub(crate) fn sandbox_denied(message: String, output: ExecToolCallOutput) -> Self {
        Self::SandboxDenied { message, output }
    }
}
