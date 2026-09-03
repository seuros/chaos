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

pub(crate) const SUPERVISED_SUBAGENT_INSTRUCTIONS: &str = "\
<supervision>
You are a materialized subagent under a supervisor. Execute the assigned task.
`send_to_supervisor` is your uplink for blockers, questions, plan-changing progress, or early findings; it queues without interrupting the supervisor.
Your final response returns automatically.
If `spawn_agent` is available, you may materialize child agents. You supervise them, and they report directly to you.
</supervision>";
