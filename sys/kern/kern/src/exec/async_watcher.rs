use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::time::Duration;
use tokio::time::Instant;

use super::ExecContext;
use super::process::ExecProcess;
use crate::chaos::Session;
use crate::chaos::TurnContext;
use crate::exec::ExecToolCallOutput;
use crate::exec::MAX_EXEC_OUTPUT_DELTAS_PER_CALL;
use crate::exec::StreamOutput;
use crate::exec::head_tail_buffer::HeadTailBuffer;
use crate::exec::output_lifecycle::{Stream, StreamData};
use crate::protocol::EventMsg;
use crate::protocol::ExecCommandOutputDeltaEvent;
use crate::protocol::ExecCommandSource;
use crate::protocol::ExecOutputStream;
use crate::tools::events::ToolEmitter;
use crate::tools::events::ToolEventCtx;
use crate::tools::events::ToolEventStage;

pub(crate) const TRAILING_OUTPUT_GRACE: Duration = Duration::from_millis(100);

/// Upper bound for a single ExecCommandOutputDelta chunk emitted by managed exec.
///
/// The exec output buffer already caps *retained* output (see
/// `EXEC_OUTPUT_MAX_BYTES`), but we also cap per-event payload size so
/// downstream event consumers (especially app-server JSON-RPC) don't have to
/// process arbitrarily large delta payloads.
const EXEC_OUTPUT_DELTA_MAX_BYTES: usize = 8192;

/// Spawn a background task that continuously reads from the PTY, appends to the
/// shared transcript, and emits ExecCommandOutputDelta events on UTF‑8
/// boundaries.
pub(crate) fn start_streaming_output(
    process: &ExecProcess,
    context: &ExecContext,
    transcript: Arc<Mutex<HeadTailBuffer>>,
) {
    let receiver = process.output_receiver();
    let output_drained = process.output_drained_notify();
    let exit_token = process.cancellation_token();

    let session_ref = Arc::clone(&context.session);
    let turn_ref = Arc::clone(&context.turn);
    let call_id = context.call_id.clone();

    tokio::spawn(async move {
        use tokio::sync::broadcast::error::RecvError;

        let mut stream = Stream::new(StreamData {
            receiver,
            pending: Vec::new(),
            emitted_deltas: 0,
            transcript,
            session: session_ref,
            turn: turn_ref,
            call_id,
        });

        enum StreamEvent {
            Exit,
            Deadline,
            Received(Result<Vec<u8>, RecvError>),
        }

        loop {
            let deadline = stream.trailing_deadline();
            let event = tokio::select! {
                _ = exit_token.cancelled(), if deadline.is_none() => StreamEvent::Exit,
                _ = async {
                    if let Some(at) = deadline {
                        tokio::time::sleep_until(at).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => StreamEvent::Deadline,
                received = stream.data_mut().receiver.recv() => StreamEvent::Received(received),
            };
            match event {
                StreamEvent::Exit => stream.observe_exit(Instant::now() + TRAILING_OUTPUT_GRACE),
                StreamEvent::Deadline => break,
                StreamEvent::Received(received) => {
                    let chunk = match received {
                        Ok(chunk) => chunk,
                        Err(RecvError::Lagged(_)) => {
                            continue;
                        }
                        Err(RecvError::Closed) => {
                            break;
                        }
                    };

                    let data = stream.data_mut();
                    process_chunk(
                        &mut data.pending,
                        &data.transcript,
                        &data.call_id,
                        &data.session,
                        &data.turn,
                        &mut data.emitted_deltas,
                        chunk,
                    )
                    .await;
                }
            }
        }
        stream.finish();
        output_drained.notify_one();
    });
}

/// Spawn a background watcher that waits for the PTY to exit and then emits a
/// single ExecCommandEnd event with the aggregated transcript.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_exit_watcher(
    process: Arc<ExecProcess>,
    session_ref: Arc<Session>,
    turn_ref: Arc<TurnContext>,
    call_id: String,
    command: Vec<String>,
    cwd: PathBuf,
    process_id: i32,
    transcript: Arc<Mutex<HeadTailBuffer>>,
    started_at: Instant,
    completion: tokio::sync::watch::Sender<super::ExecTaskSnapshot>,
) {
    let exit_token = process.cancellation_token();
    let output_drained = process.output_drained_notify();

    tokio::spawn(async move {
        exit_token.cancelled().await;
        output_drained.notified().await;

        let exit_code = process.exit_code().unwrap_or(-1);
        let duration = Instant::now().saturating_duration_since(started_at);
        completion.send_replace(super::ExecTaskSnapshot::Exited {
            exit_code: process.exit_code(),
            command: command.clone(),
            output: transcript.lock().await.to_bytes(),
            wall_time: duration,
        });
        emit_exec_end(
            session_ref,
            turn_ref,
            call_id,
            command,
            cwd,
            Some(process_id.to_string()),
            transcript,
            String::new(),
            exit_code,
            duration,
        )
        .await;
    });
}

async fn process_chunk(
    pending: &mut Vec<u8>,
    transcript: &Arc<Mutex<HeadTailBuffer>>,
    call_id: &str,
    session_ref: &Arc<Session>,
    turn_ref: &Arc<TurnContext>,
    emitted_deltas: &mut usize,
    chunk: Vec<u8>,
) {
    pending.extend_from_slice(&chunk);
    while let Some(prefix) = split_valid_utf8_prefix(pending) {
        {
            let mut guard = transcript.lock().await;
            guard.push_chunk(prefix.to_vec());
        }

        if *emitted_deltas >= MAX_EXEC_OUTPUT_DELTAS_PER_CALL {
            continue;
        }

        let event = ExecCommandOutputDeltaEvent {
            call_id: call_id.to_string(),
            stream: ExecOutputStream::Stdout,
            chunk: prefix,
        };
        session_ref
            .send_event(turn_ref.as_ref(), EventMsg::ExecCommandOutputDelta(event))
            .await;
        *emitted_deltas += 1;
    }
}

/// Emit an ExecCommandEnd event for a managed exec session, using the transcript
/// as the primary source of aggregated_output and falling back to the provided
/// text when the transcript is empty.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn emit_exec_end(
    session_ref: Arc<Session>,
    turn_ref: Arc<TurnContext>,
    call_id: String,
    command: Vec<String>,
    cwd: PathBuf,
    process_id: Option<String>,
    transcript: Arc<Mutex<HeadTailBuffer>>,
    fallback_output: String,
    exit_code: i32,
    duration: Duration,
) {
    let aggregated_output = resolve_aggregated_output(&transcript, fallback_output).await;
    let output = ExecToolCallOutput {
        exit_code,
        stdout: StreamOutput::new(aggregated_output.clone()),
        stderr: StreamOutput::new(String::new()),
        aggregated_output: StreamOutput::new(aggregated_output),
        duration,
        timed_out: false,
    };
    let event_ctx = ToolEventCtx::new(
        session_ref.as_ref(),
        turn_ref.as_ref(),
        &call_id,
        /*turn_diff_tracker*/ None,
    );
    let emitter = ToolEmitter::exec(&command, cwd, ExecCommandSource::ExecStartup, process_id);
    emitter
        .emit(event_ctx, ToolEventStage::Success(output))
        .await;
}

fn split_valid_utf8_prefix(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    split_valid_utf8_prefix_with_max(buffer, EXEC_OUTPUT_DELTA_MAX_BYTES)
}

fn split_valid_utf8_prefix_with_max(buffer: &mut Vec<u8>, max_bytes: usize) -> Option<Vec<u8>> {
    if buffer.is_empty() {
        return None;
    }

    let max_len = buffer.len().min(max_bytes);
    let mut split = max_len;
    while split > 0 {
        if std::str::from_utf8(&buffer[..split]).is_ok() {
            let prefix = buffer[..split].to_vec();
            buffer.drain(..split);
            return Some(prefix);
        }

        if max_len - split > 4 {
            break;
        }
        split -= 1;
    }

    // If no valid UTF-8 prefix was found, emit the first byte so the stream
    // keeps making progress and the transcript reflects all bytes.
    let byte = buffer.drain(..1).collect();
    Some(byte)
}

async fn resolve_aggregated_output(
    transcript: &Arc<Mutex<HeadTailBuffer>>,
    fallback: String,
) -> String {
    let guard = transcript.lock().await;
    if guard.retained_bytes() == 0 {
        return fallback;
    }

    String::from_utf8_lossy(&guard.to_bytes()).to_string()
}

#[cfg(test)]
mod tests;
