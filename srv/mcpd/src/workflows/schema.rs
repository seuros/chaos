use super::Operation;
use mcp_host::prelude::ToolError;
use serde_json::{Value, json};
use std::time::Duration;

pub(super) fn description(operation: Operation) -> &'static str {
    match operation {
        Operation::Run => {
            "Execute a bounded, session-owned tree under current configured kernel policy. \
             Nodes sequence/fallback/parallel_all/race {id,children}; \
             timeout {id,timeout_ms,child}; retry {id,attempts,child} \
             <=3 attempts with affirmative tool idempotency; \
             condition {id,pointer,predicate:{op:exists|equals,value?}}; \
             call {id,tool,arguments,bindings:[{source,target}],result_key?}; \
             ask {id,message,schema?,result_key?}; \
             sample {id,prompt,system?,max_tokens?,result_key?}. \
             Unique IDs/result keys; input reserved. \
             Limits128 nodes/depth16/concurrency16/256 attempts. No controls/nested runners. \
             Ask/Sample use this host's negotiated capabilities. \
             Workflows group must already be enabled in operator config. \
             No permission/provider overrides or restart replay."
        }
        Operation::Synopsis => {
            "Only after explicit user authorization for delegation. \
             Provide jobs and exactly one of mode or tree. \
             Modes sequence/parallel_all/fallback/race. \
             Tree composites as run_workflow, but only job {id,job_id,result_key?} \
             and condition leaves. Each job appears exactly once; \
             retries require allow_replay and create fresh children. \
             <=16 jobs/128 nodes/depth16/256 attempts. \
             Configured agent policy, accounting and sandbox apply. \
             Returns owned run_id; no restart replay."
        }
        Operation::Status => {
            "Inspect only this MCP session's workflow/synopsis run, including after completion. \
             Unknown/expired runs cannot be restored."
        }
        Operation::Cancel => {
            "Cancel only this MCP session's run. \
             Cancelling is not terminal until cleanup completes."
        }
        Operation::Dashboard => {
            "App-only metadata page of this session's runs. \
             Payload details omitted; use workflow_status for full bounded snapshots. \
             At most ten runs, paginate with next_offset."
        }
        Operation::Enable => {
            "Explicitly enable capability groups for this MCP session's workflow/synopsis roots. \
             Groups: workflows, agents, shell, filesystem, editing, session, web, cron, \
             mcp-management, recall. Defaults disabled. \
             Does not change mode, sandbox or per-action approvals. \
             Controls cannot be workflow leaves."
        }
        Operation::Disable => {
            "Disable capability groups for this MCP session, \
             including policy checks in running workflows. \
             Owned status/cancellation remain available."
        }
    }
}

pub(super) fn input(operation: Operation) -> Value {
    match operation {
        Operation::Run => json!({
            "type":"object","required":["tree"],"additionalProperties":false,
            "properties":{"tree":{"type":"object"},"input":{},
                "timeout_ms":{"type":"integer","minimum":1,"maximum":3600000}}
        }),
        Operation::Synopsis => json!({
            "type":"object","required":["jobs"],"additionalProperties":false,
            "properties":{
                "tree":{"type":"object"},
                "mode":{"type":"string","enum":["sequence","parallel_all","fallback","race"]},
                "jobs":{"type":"array","minItems":1,"maxItems":16,"items":{
                    "type":"object","required":["id","message"],"additionalProperties":false,
                    "properties":{"id":{"type":"string"},"message":{"type":"string"},
                        "agent_type":{"type":"string"},"allow_replay":{"type":"boolean"}}
                }},
                "timeout_ms":{"type":"integer","minimum":1,"maximum":3600000}
            }
        }),
        Operation::Status | Operation::Cancel => json!({
            "type":"object","required":["run_id"],"additionalProperties":false,
            "properties":{"run_id":{"type":"string"}}
        }),
        Operation::Dashboard => json!({
            "type":"object","additionalProperties":false,
            "properties":{"offset":{"type":"integer","minimum":0}}
        }),
        Operation::Enable | Operation::Disable => json!({
            "type":"object","required":["groups"],"additionalProperties":false,
            "properties":{"groups":{"type":"array","minItems":1,"items":{"type":"string"}}}
        }),
    }
}

pub(super) fn deadline(operation: Operation, params: &Value) -> Result<Duration, ToolError> {
    let default = if matches!(operation, Operation::Synopsis) {
        1_800_000
    } else {
        120_000
    };
    let ms = params
        .get("timeout_ms")
        .map(|value| {
            value.as_u64().filter(|ms| *ms > 0).ok_or_else(|| {
                ToolError::InvalidArguments("timeout_ms must be positive milliseconds".into())
            })
        })
        .transpose()?
        .unwrap_or(default);
    if ms > 3_600_000 {
        return Err(ToolError::InvalidArguments(
            "timeout_ms must not exceed one hour".into(),
        ));
    }
    // Startup/approval-context admission and cleanup are bounded separately
    // from the existing tree deadline; no unbounded optional-task handlers.
    Ok(
        Duration::from_millis(ms.max(if matches!(operation, Operation::Synopsis) {
            10_000
        } else {
            1
        })) + Duration::from_secs(40),
    )
}
