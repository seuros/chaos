use chaos_synopsis::runs::{RunRecorder, StepOutcome};
use mcp_host::bt::{Status, StepInfo, TreeContext, TreeObserver};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use tokio_util::sync::CancellationToken;

/// Shared run-recording adapter for tool and agent trees.
pub(crate) struct Observer {
    recorder: RunRecorder,
    cancellation: CancellationToken,
    attempts: Mutex<BTreeMap<String, usize>>,
    display_names: BTreeMap<String, String>,
}

impl Observer {
    pub(crate) fn new(
        recorder: RunRecorder,
        cancellation: CancellationToken,
        display_names: BTreeMap<String, String>,
    ) -> Self {
        Self {
            recorder,
            cancellation,
            attempts: Mutex::new(BTreeMap::new()),
            display_names,
        }
    }
    /// Rewrite only SDK-owned labels, never arbitrary values inside tool output.
    pub(crate) fn display_record(&self, record: &mut Value) {
        if let Some(name) = record.get("tool").and_then(Value::as_str)
            && let Some(canonical) = self.display_names.get(name)
        {
            record["tool"] = Value::String(canonical.clone());
        }
    }
    pub(crate) fn display_result(&self, result: &mut Value) {
        if let Some(steps) = result.get_mut("steps").and_then(Value::as_array_mut) {
            for record in steps {
                self.display_record(record);
            }
        }
    }
    pub(crate) fn attempt(&self, id: &str) -> Option<usize> {
        self.attempts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .copied()
    }
}

impl TreeObserver for Observer {
    fn started(&self, _: TreeContext<'_>, step: &StepInfo, attempt: usize) {
        if let Some(id) = step.id.as_deref()
            && self.recorder.start_step(id, attempt).is_ok()
        {
            self.attempts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(id.into(), attempt);
        } else {
            self.cancellation.cancel();
        }
    }
    fn waiting_for_input(&self, _: TreeContext<'_>, step: &StepInfo, attempt: usize) {
        if let Some(id) = step.id.as_deref()
            && self.recorder.waiting_for_input(id, attempt).is_err()
        {
            self.cancellation.cancel();
        }
    }
    fn finished(
        &self,
        _: TreeContext<'_>,
        step: &StepInfo,
        attempt: usize,
        status: Status,
        record: &Value,
    ) {
        if let Some(id) = step.id.as_deref() {
            let outcome = if status == Status::Success {
                StepOutcome::Success
            } else {
                StepOutcome::Failure
            };
            // Private leaf adapters return ordinary dispatch output, governed
            // by its existing output policy and the registry's total bound.
            let mut record = record.clone();
            self.display_record(&mut record);
            if self
                .recorder
                .finish_step(id, attempt, outcome, Some(record))
                .is_err()
            {
                self.cancellation.cancel();
            }
        }
    }
    fn halted(&self, _: TreeContext<'_>, step: &StepInfo, attempt: usize) {
        if let Some(id) = step.id.as_deref() {
            // An already-terminal attempt may race with SDK scope cleanup.
            let _ = self
                .recorder
                .finish_step(id, attempt, StepOutcome::Cancelled, None);
        }
    }
}
