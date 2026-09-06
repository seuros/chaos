pub(crate) fn format_subagent_context_line(agent_id: &str, agent_nickname: Option<&str>) -> String {
    match agent_nickname.filter(|nickname| !nickname.is_empty()) {
        Some(agent_nickname) => format!("- {agent_id}: {agent_nickname}"),
        None => format!("- {agent_id}"),
    }
}
