use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chaos_abi::AbiError;
use chaos_abi::TurnEvent;
use chaos_libration::UsageSniffer;
use rama::bytes::Bytes;
use rama::error::BoxError;
use rama::futures;
use rama::http::HeaderMap;
use rama::http::body::util::BodyExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::parse::TextAccumulator;
use super::parse::ToolCallAccumulator;
use super::parse::parse_chunk;
use crate::provider::Provider;

pub(super) async fn run_response(
    url: &str,
    headers: &HeaderMap,
    body: &Value,
    provider: &Provider,
    sniffer: Option<&Arc<UsageSniffer>>,
    tx: mpsc::Sender<Result<TurnEvent, AbiError>>,
) -> Result<(), AbiError> {
    let response = crate::sse::transport::start_rama_post_sse_request(
        url,
        headers,
        body,
        provider,
        "chat_completions",
        sniffer,
    )
    .await?;
    if body.get("stream").and_then(Value::as_bool) == Some(true) {
        process_sse_data_stream(
            response.into_body().into_data_stream(),
            provider.stream_idle_timeout,
            tx,
        )
        .await
    } else {
        process_json_response(response.into_body(), provider.stream_idle_timeout, tx).await
    }
}

pub(crate) async fn process_json_response(
    body: rama::http::Body,
    response_timeout: Duration,
    tx: mpsc::Sender<Result<TurnEvent, AbiError>>,
) -> Result<(), AbiError> {
    let bytes = timeout(response_timeout, body.collect())
        .await
        .map_err(|_| AbiError::Stream("timeout waiting for JSON response".to_string()))?
        .map_err(|e| AbiError::Transport {
            status: 0,
            message: e.to_string(),
        })?
        .to_bytes();

    let json = serde_json::from_slice::<Value>(&bytes)
        .map_err(|e| AbiError::Stream(format!("chat completions returned invalid JSON: {e}")))?;

    for event in parse_completion(&json)? {
        if tx.send(Ok(event)).await.is_err() {
            break;
        }
    }
    Ok(())
}

pub(crate) fn parse_completion(json: &Value) -> Result<Vec<TurnEvent>, AbiError> {
    let choices = json
        .get("choices")
        .and_then(Value::as_array)
        .filter(|choices| choices.len() == 1)
        .ok_or_else(|| AbiError::Stream("expected one chat completion choice".to_string()))?;
    let choice = &choices[0];
    let message = choice
        .get("message")
        .filter(|message| message.is_object())
        .ok_or_else(|| AbiError::Stream("chat completion is missing its message".to_string()))?;
    if message
        .get("refusal")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    {
        return Err(AbiError::Stream(
            "chat completion refused structured output".to_string(),
        ));
    }
    match choice.get("finish_reason").and_then(Value::as_str) {
        Some("stop" | "tool_calls") => {}
        reason => {
            return Err(AbiError::Stream(format!(
                "incomplete chat completion: {reason:?}"
            )));
        }
    }
    if !message
        .get("content")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
        && !message
            .get("tool_calls")
            .and_then(Value::as_array)
            .is_some_and(|calls| !calls.is_empty())
    {
        return Err(AbiError::Stream(
            "chat completion has no text or tool calls".to_string(),
        ));
    }
    let mut response_id = String::new();
    let mut events = parse_chunk(
        json,
        &mut BTreeMap::new(),
        &mut None,
        &mut response_id,
        &mut None,
    )?;
    if !events
        .iter()
        .any(|event| matches!(event, TurnEvent::Completed { .. }))
    {
        events.push(TurnEvent::Completed {
            response_id,
            token_usage: None,
        });
    }
    Ok(events)
}

pub(crate) async fn process_sse_data_stream<S, E>(
    data_stream: S,
    idle_timeout: Duration,
    tx: mpsc::Sender<Result<TurnEvent, AbiError>>,
) -> Result<(), AbiError>
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
    E: Into<BoxError> + std::fmt::Display,
{
    let mut tool_acc: BTreeMap<usize, ToolCallAccumulator> = BTreeMap::new();
    let mut text_acc: Option<TextAccumulator> = None;
    let mut server_model: Option<String> = None;
    crate::sse::process_json_sse(data_stream, idle_timeout, tx, |json, response_id| {
        parse_chunk(
            json,
            &mut tool_acc,
            &mut text_acc,
            response_id,
            &mut server_model,
        )
    })
    .await
}
