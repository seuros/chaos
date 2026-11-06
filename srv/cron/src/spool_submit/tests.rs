use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use super::*;
use chaos_abi::SpoolBackend;
use chaos_abi::SpoolError;
use chaos_abi::SpoolItem;
use chaos_abi::SpoolPhase;
use chaos_abi::SpoolStatusReport;

mod compensate;
mod resume;
mod submit;

struct RecordingBackend {
    submitted: Mutex<Vec<Vec<String>>>,
    cancelled: Mutex<Vec<String>>,
}

impl RecordingBackend {
    fn new() -> Self {
        Self {
            submitted: Mutex::new(Vec::new()),
            cancelled: Mutex::new(Vec::new()),
        }
    }
}

struct FailingBackend;

impl SpoolBackend for RecordingBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn submit(
        &self,
        items: Vec<(String, TurnRequest)>,
    ) -> Pin<Box<dyn Future<Output = Result<String, SpoolError>> + Send + '_>> {
        let ids: Vec<String> = items.into_iter().map(|(id, _)| id).collect();
        self.submitted.lock().expect("poison").push(ids);
        Box::pin(async { Ok("mock-batch-42".into()) })
    }

    fn poll(
        &self,
        _batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<SpoolStatusReport, SpoolError>> + Send + '_>> {
        Box::pin(async {
            Ok(SpoolStatusReport {
                phase: SpoolPhase::InProgress,
                raw_provider_status: "mock".into(),
            })
        })
    }

    fn fetch_results(
        &self,
        _batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SpoolItem>, SpoolError>> + Send + '_>> {
        Box::pin(async { Ok(vec![]) })
    }

    fn cancel(
        &self,
        batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), SpoolError>> + Send + '_>> {
        self.cancelled
            .lock()
            .expect("poison")
            .push(batch_id.to_string());
        Box::pin(async { Ok(()) })
    }
}

impl SpoolBackend for FailingBackend {
    fn name(&self) -> &'static str {
        "failing"
    }

    fn submit(
        &self,
        _items: Vec<(String, TurnRequest)>,
    ) -> Pin<Box<dyn Future<Output = Result<String, SpoolError>> + Send + '_>> {
        Box::pin(async { Err(SpoolError::Other("submit exploded".into())) })
    }

    fn poll(
        &self,
        _batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<SpoolStatusReport, SpoolError>> + Send + '_>> {
        Box::pin(async {
            Ok(SpoolStatusReport {
                phase: SpoolPhase::InProgress,
                raw_provider_status: "unreachable".into(),
            })
        })
    }

    fn fetch_results(
        &self,
        _batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SpoolItem>, SpoolError>> + Send + '_>> {
        Box::pin(async { Ok(vec![]) })
    }

    fn cancel(
        &self,
        _batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), SpoolError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

fn dummy_turn_request(model: &str) -> TurnRequest {
    TurnRequest {
        model: model.into(),
        instructions: String::new(),
        input: vec![],
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: serde_json::Map::new(),
    }
}

fn poll_wiring(name: &str) -> PollWiring {
    PollWiring {
        name: name.into(),
        schedule_json: crate::schedule::Schedule::Interval { seconds: 300 }.to_json(),
        project_path: "/tmp/project".into(),
    }
}
