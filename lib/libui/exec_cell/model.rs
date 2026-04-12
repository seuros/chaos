//! Data model for grouped exec-call history cells in the TUI transcript.
//!
//! An `ExecCell` can represent either a single command or an "exploring" group of related read/
//! list/search commands. The chat widget relies on stable `call_id` matching to route progress and
//! end events into the right cell, and it treats "call id not found" as a real signal (for
//! example, an orphan end that should render as a separate history entry).

use std::time::Duration;
use std::time::Instant;

use chaos_ipc::parse_command::ParsedCommand;
use chaos_ipc::protocol::ExecCommandSource;

#[derive(Clone, Debug, Default)]
pub struct CommandOutput {
    pub exit_code: i32,
    /// The aggregated stderr + stdout interleaved.
    pub aggregated_output: String,
    /// The formatted output of the command, as seen by the model.
    pub formatted_output: String,
}

#[derive(Debug, Clone)]
pub struct ExecCall {
    pub call_id: String,
    pub command: Vec<String>,
    pub parsed: Vec<ParsedCommand>,
    pub output: Option<CommandOutput>,
    pub source: ExecCommandSource,
    pub start_time: Option<Instant>,
    pub duration: Option<Duration>,
    pub interaction_input: Option<String>,
}

#[derive(Debug)]
pub struct ExecCell {
    pub calls: Vec<ExecCall>,
    animations_enabled: bool,
}

impl ExecCell {
    pub fn new(call: ExecCall, animations_enabled: bool) -> Self {
        Self {
            calls: vec![call],
            animations_enabled,
        }
    }

    pub fn with_added_call(
        &self,
        call_id: String,
        command: Vec<String>,
        parsed: Vec<ParsedCommand>,
        source: ExecCommandSource,
        interaction_input: Option<String>,
    ) -> Option<Self> {
        let call = ExecCall {
            call_id,
            command,
            parsed,
            output: None,
            source,
            start_time: Some(Instant::now()),
            duration: None,
            interaction_input,
        };
        if self.is_exploring_cell() && Self::is_exploring_call(&call) {
            Some(Self {
                calls: [self.calls.clone(), vec![call]].concat(),
                animations_enabled: self.animations_enabled,
            })
        } else {
            None
        }
    }

    /// Marks the most recently matching call as finished and returns whether a call was found.
    ///
    /// Callers should treat `false` as a routing mismatch rather than silently ignoring it. The
    /// chat widget uses that signal to avoid attaching an orphan `exec_end` event to an unrelated
    /// active exploring cell, which would incorrectly collapse two transcript entries together.
    pub fn complete_call(
        &mut self,
        call_id: &str,
        output: CommandOutput,
        duration: Duration,
    ) -> bool {
        let Some(call) = self.calls.iter_mut().rev().find(|c| c.call_id == call_id) else {
            return false;
        };
        call.output = Some(output);
        call.duration = Some(duration);
        call.start_time = None;
        true
    }

    pub fn should_flush(&self) -> bool {
        !self.is_exploring_cell() && self.calls.iter().all(|c| c.output.is_some())
    }

    pub fn mark_failed(&mut self) {
        for call in self.calls.iter_mut() {
            if call.output.is_none() {
                let elapsed = call
                    .start_time
                    .map(|st| st.elapsed())
                    .unwrap_or_else(|| Duration::from_millis(0));
                call.start_time = None;
                call.duration = Some(elapsed);
                call.output = Some(CommandOutput {
                    exit_code: 1,
                    formatted_output: String::new(),
                    aggregated_output: String::new(),
                });
            }
        }
    }

    pub fn is_exploring_cell(&self) -> bool {
        self.calls.iter().all(Self::is_exploring_call)
    }

    pub fn is_active(&self) -> bool {
        self.calls.iter().any(|c| c.output.is_none())
    }

    pub fn active_start_time(&self) -> Option<Instant> {
        self.calls
            .iter()
            .find(|c| c.output.is_none())
            .and_then(|c| c.start_time)
    }

    pub fn animations_enabled(&self) -> bool {
        self.animations_enabled
    }

    pub fn iter_calls(&self) -> impl Iterator<Item = &ExecCall> {
        self.calls.iter()
    }

    pub fn append_output(&mut self, call_id: &str, chunk: &str) -> bool {
        if chunk.is_empty() {
            return false;
        }
        let Some(call) = self.calls.iter_mut().rev().find(|c| c.call_id == call_id) else {
            return false;
        };
        let output = call.output.get_or_insert_with(CommandOutput::default);
        output.aggregated_output.push_str(chunk);
        true
    }

    pub(super) fn is_exploring_call(call: &ExecCall) -> bool {
        !matches!(call.source, ExecCommandSource::UserShell)
            && !call.parsed.is_empty()
            && call.parsed.iter().all(|p| {
                matches!(
                    p,
                    ParsedCommand::Read { .. }
                        | ParsedCommand::ListFiles { .. }
                        | ParsedCommand::Search { .. }
                )
            })
    }
}

impl ExecCall {
    pub fn is_user_shell_command(&self) -> bool {
        matches!(self.source, ExecCommandSource::UserShell)
    }

    pub fn is_unified_exec_interaction(&self) -> bool {
        matches!(self.source, ExecCommandSource::UnifiedExecInteraction)
    }
}
