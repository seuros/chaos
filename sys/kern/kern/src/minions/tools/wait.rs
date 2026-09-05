use super::common::get_agent_info;
use super::common::impl_function_tool_kind;
use super::common::impl_tool_output;
use super::{
    AgentStatus, Arc, ChaosErr, CollabAgentRef, CollabWaitingBeginEvent, CollabWaitingEndEvent,
    DEFAULT_WAIT_TIMEOUT_MS, FunctionCallError, MAX_WAIT_TIMEOUT_MS, MIN_WAIT_TIMEOUT_MS,
    ProcessId, ResponseInputItem, Serialize, Session, ToolHandler, ToolInvocation, ToolKind,
    ToolOutput, ToolPayload, agent_id, build_wait_agent_statuses, collab_agent_error,
    function_arguments, parse_arguments, tool_output_json_text, tool_output_response_item,
};
use crate::minions::status::is_final;
use futures::FutureExt;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::watch::Receiver;
use tokio::time::Instant;

use tokio::time::timeout_at;

pub(crate) struct Handler;

impl ToolHandler for Handler {
    type Output = WaitAgentResult;

    impl_function_tool_kind!();

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            call_id,
            ..
        } = invocation;
        let arguments = function_arguments(payload)?;
        let args: WaitArgs = parse_arguments(&arguments)?;
        if args.ids.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "ids must be non-empty".to_owned(),
            ));
        }
        let receiver_process_ids = args
            .ids
            .iter()
            .map(|id| agent_id(id))
            .collect::<Result<Vec<_>, _>>()?;
        let mut receiver_agents = Vec::with_capacity(receiver_process_ids.len());
        for receiver_process_id in &receiver_process_ids {
            let (agent_nickname, agent_role) = get_agent_info(&session, *receiver_process_id).await;
            receiver_agents.push(CollabAgentRef {
                process_id: *receiver_process_id,
                agent_nickname,
                agent_role,
            });
        }

        let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let timeout_ms = match timeout_ms {
            ms if ms <= 0 => {
                return Err(FunctionCallError::RespondToModel(
                    "timeout_ms must be greater than zero".to_owned(),
                ));
            }
            ms => ms.clamp(MIN_WAIT_TIMEOUT_MS, MAX_WAIT_TIMEOUT_MS),
        };

        session
            .send_event(
                &turn,
                CollabWaitingBeginEvent {
                    sender_process_id: session.conversation_id,
                    receiver_process_ids: receiver_process_ids.clone(),
                    receiver_agents: receiver_agents.clone(),
                    call_id: call_id.clone(),
                }
                .into(),
            )
            .await;

        let mut status_rxs = Vec::with_capacity(receiver_process_ids.len());
        let mut initial_final_statuses = Vec::new();
        for id in &receiver_process_ids {
            match session.services.agent_control.subscribe_status(*id).await {
                Ok(rx) => {
                    let status = rx.borrow().clone();
                    if is_final(&status) {
                        initial_final_statuses.push((*id, status));
                    }
                    status_rxs.push((*id, rx));
                }
                Err(ChaosErr::ProcessNotFound(_)) => {
                    initial_final_statuses.push((*id, AgentStatus::NotFound));
                }
                Err(err) => {
                    let mut statuses = HashMap::with_capacity(1);
                    statuses.insert(*id, session.services.agent_control.get_status(*id).await);
                    session
                        .send_event(
                            &turn,
                            CollabWaitingEndEvent {
                                sender_process_id: session.conversation_id,
                                call_id: call_id.clone(),
                                agent_statuses: build_wait_agent_statuses(
                                    &statuses,
                                    &receiver_agents,
                                ),
                                statuses,
                            }
                            .into(),
                        )
                        .await;
                    return Err(collab_agent_error(*id, err));
                }
            }
        }

        let statuses = if !initial_final_statuses.is_empty() {
            initial_final_statuses
        } else {
            let mut futures = FuturesUnordered::new();
            for (id, rx) in status_rxs.into_iter() {
                let session = session.clone();
                futures.push(wait_for_final_status(session, id, rx));
            }
            let mut results = Vec::new();
            let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
            loop {
                match timeout_at(deadline, futures.next()).await {
                    Ok(Some(Some(result))) => {
                        results.push(result);
                        break;
                    }
                    Ok(Some(None)) => continue,
                    Ok(None) | Err(_) => break,
                }
            }
            if !results.is_empty() {
                loop {
                    match futures.next().now_or_never() {
                        Some(Some(Some(result))) => results.push(result),
                        Some(Some(None)) => continue,
                        Some(None) | None => break,
                    }
                }
            }
            results
        };

        let statuses_map = statuses.clone().into_iter().collect::<HashMap<_, _>>();
        let agent_statuses = build_wait_agent_statuses(&statuses_map, &receiver_agents);
        let result = WaitAgentResult {
            status: statuses_map.clone(),
            timed_out: statuses.is_empty(),
        };

        session
            .send_event(
                &turn,
                CollabWaitingEndEvent {
                    sender_process_id: session.conversation_id,
                    call_id,
                    agent_statuses,
                    statuses: statuses_map,
                }
                .into(),
            )
            .await;

        Ok(result)
    }
}

#[derive(Debug, Deserialize)]
struct WaitArgs {
    ids: Vec<String>,
    timeout_ms: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct WaitAgentResult {
    pub(crate) status: HashMap<ProcessId, AgentStatus>,
    pub(crate) timed_out: bool,
}

impl_tool_output!(WaitAgentResult, "wait_agent", true, None);

async fn wait_for_final_status(
    session: Arc<Session>,
    process_id: ProcessId,
    mut status_rx: Receiver<AgentStatus>,
) -> Option<(ProcessId, AgentStatus)> {
    let mut status = status_rx.borrow().clone();
    if is_final(&status) {
        return Some((process_id, status));
    }

    loop {
        if status_rx.changed().await.is_err() {
            let latest = session.services.agent_control.get_status(process_id).await;
            return is_final(&latest).then_some((process_id, latest));
        }
        status = status_rx.borrow().clone();
        if is_final(&status) {
            return Some((process_id, status));
        }
    }
}
