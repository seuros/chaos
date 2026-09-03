pub(crate) mod control;
mod guards;
pub(crate) mod role;
pub(crate) mod router;
pub(crate) mod status;
pub(crate) mod tools;

pub(crate) use chaos_ipc::protocol::AgentStatus;
pub(crate) use control::AgentControl;
pub(crate) use guards::exceeds_process_spawn_depth_limit;
pub(crate) use guards::next_process_spawn_depth;
pub(crate) use status::agent_status_from_event;

const INTERNAL_AGENT_ROLE_PREFIX: &str = "__chaos_internal__:";

pub(crate) fn internal_agent_role(kind: &str, identity: &str) -> String {
    format!("{INTERNAL_AGENT_ROLE_PREFIX}{kind}:{identity}")
}

pub(crate) fn is_internal_process_spawn(source: &chaos_ipc::protocol::SessionSource) -> bool {
    matches!(
        source,
        chaos_ipc::protocol::SessionSource::SubAgent(
            chaos_ipc::protocol::SubAgentSource::ProcessSpawn {
                agent_role: Some(role),
                ..
            }
        ) if role.starts_with(INTERNAL_AGENT_ROLE_PREFIX)
    )
}

pub(crate) const SUPERVISED_SUBAGENT_INSTRUCTIONS: &str = "\
<supervision>
You are a materialized subagent under a supervisor. Execute the assigned task.
`send_to_supervisor` is your uplink for blockers, questions, plan-changing progress, or early findings; it queues without interrupting the supervisor.
Your final response returns automatically.
If `spawn_agent` is available, you may materialize child agents. You supervise them, and they report directly to you.
</supervision>";
