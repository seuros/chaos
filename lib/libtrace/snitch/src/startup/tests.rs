use super::StartupTimeline;
use crate::tracing;
use crate::tracing::subscriber as tracing_subscriber;
use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn startup_log_replays_early_milestones_once_and_reports_later_frames() {
    let output = Output::default();
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::dispatcher::with_default(&tracing::Dispatch::new(subscriber), || {
        let timeline = StartupTimeline::new("console");
        timeline.mark("frontend_entry");
        assert!(output.0.lock().unwrap().is_empty());
        timeline.enable_logging();
        timeline.mark("first_frame");
        timeline.mark("first_frame");
        timeline.enable_logging();
    });
    let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    let records: Vec<_> = log.lines().collect();
    assert_eq!(records.len(), 2, "one log record per real milestone: {log}");
    assert!(records[0].contains("frontend_entry"));
    assert!(records[1].contains("first_frame"));
    assert!(records.iter().all(|line| line.contains("console")
        && line.contains("elapsed_ms=")
        && line.contains("duration_ms=")));
}
