//! A session's MCP servers, lit while one of their tools runs.

use chaos_kern::activity::Station;
use serde::Serialize;

use super::super::board::Session;
use super::super::view::name;

#[derive(Serialize)]
pub(in crate::monitor) struct McpChip {
    pub name: String,
    pub state: String,
    pub busy: bool,
}

pub(super) fn chips(session: &Session) -> Vec<McpChip> {
    session
        .mcp
        .iter()
        .map(|(server, state)| McpChip {
            name: server.clone(),
            state: name(state),
            busy: session.calls.values().any(|call| {
                call.station == Station::Mcp
                    && call
                        .label
                        .strip_prefix(server.as_str())
                        .is_some_and(|tool| tool.starts_with('.'))
            }),
        })
        .collect()
}
