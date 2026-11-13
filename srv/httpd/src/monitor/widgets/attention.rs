//! What deserves a look, worst first. Every rule reads only the board.

use chaos_kern::activity::MachinePhase;
use serde::Serialize;

use super::super::board::{self, Board};
use super::super::view::{Widget, name};
use super::short;

const SLOW_CALL_MS: i64 = 60_000;
const FAILING_IN_A_ROW: u32 = 3;
const CONTEXT_HIGH: i64 = 80;
const CACHE_LOW: i64 = 40;
const CACHE_MIN_INPUT: i64 = 30_000;

#[derive(Serialize)]
pub(in crate::monitor) struct Attention {
    pub items: Vec<Item>,
}

#[derive(Serialize)]
pub(in crate::monitor) struct Item {
    pub tone: &'static str,
    pub title: String,
    pub detail: String,
    pub session: Option<String>,
    #[serde(skip)]
    rank: u8,
}

impl Widget for Attention {
    const NAME: &'static str = "attention";
}

impl Attention {
    pub(in crate::monitor) fn from(board: &Board, now_ms: i64) -> Self {
        let mut items = Vec::new();
        let mut push = |rank, tone, title: String, detail: String, session: Option<String>| {
            items.push(Item {
                tone,
                title,
                detail,
                session,
                rank,
            });
        };
        if let Some((phase, causes)) = &board.machine
            && !matches!(phase, MachinePhase::Healthy | MachinePhase::Recovered)
        {
            let causes: Vec<_> = causes.iter().map(name).collect();
            push(
                0,
                "bad",
                "agents paused".into(),
                format!("machine {}: {}", name(phase), causes.join(", ")),
                None,
            );
        }
        for (id, session) in &board.sessions {
            let who = Some(short(&id.to_string()));
            for reason in session.asks.values() {
                push(
                    1,
                    "ask",
                    "waiting on you".into(),
                    board::attention(*reason).into(),
                    who.clone(),
                );
            }
            if let Some(kind) = session.failed_check {
                push(
                    2,
                    "bad",
                    format!("failing {}", name(&kind)),
                    "until a run of the same kind passes".into(),
                    who.clone(),
                );
            }
            if session.failing >= FAILING_IN_A_ROW {
                push(
                    2,
                    "bad",
                    format!("{} failures in a row", session.failing),
                    "the agent may be stuck".into(),
                    who.clone(),
                );
            }
            for call in session.calls.values() {
                let running = now_ms - call.started_ms;
                if running >= SLOW_CALL_MS {
                    push(
                        3,
                        "ask",
                        "slow call".into(),
                        format!(
                            "{} {} running {}",
                            name(&call.station),
                            call.label,
                            board::duration(u64::try_from(running).unwrap_or(0))
                        ),
                        who.clone(),
                    );
                }
            }
            if let Some((used, Some(window))) = session.context
                && window > 0
                && used * 100 / window >= CONTEXT_HIGH
            {
                push(
                    3,
                    "ask",
                    "context nearly full".into(),
                    format!("{}% used", used * 100 / window),
                    who.clone(),
                );
            }
            let input = session.usage.input;
            if input >= CACHE_MIN_INPUT && session.usage.cache_read * 100 / input < CACHE_LOW {
                push(
                    4,
                    "quiet",
                    "low cache hit".into(),
                    format!(
                        "{}% of input from cache",
                        session.usage.cache_read * 100 / input
                    ),
                    who.clone(),
                );
            }
        }
        items.sort_by_key(|item| item.rank);
        Self { items }
    }
}

#[cfg(test)]
mod tests;
