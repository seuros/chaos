use std::path::Path;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context as _;
use anyhow::Result;
use supervisor_machines::Decision;
use supervisor_machines::Exit;
use supervisor_machines::Intensity;
use supervisor_machines::Policy;
use supervisor_machines::Strategy;
use supervisor_machines::Supervisor;

pub(super) const HISTORY_FILENAME: &str = "journald.restarts";
const MAX_RESTARTS: u32 = 5;
const WINDOW: Duration = Duration::from_secs(60);
const BASE_DELAY: Duration = Duration::from_millis(100);
const MAX_DELAY: Duration = Duration::from_secs(5);

type RestartSupervisor = Supervisor<1, 8>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Verdict {
    Spawn { delay: Duration },
    Refuse { recent: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RestartDecision {
    pub verdict: Verdict,
    pub history: Vec<Duration>,
}

pub(super) fn decide(
    history: impl IntoIterator<Item = Duration>,
    now: Duration,
) -> Result<RestartDecision> {
    let mut supervisor = Intensity::new(MAX_RESTARTS, WINDOW)
        .and_then(|intensity| {
            RestartSupervisor::restore(Strategy::OneForOne, intensity, history, now)
        })
        .context("journald restart supervisor shape is invalid")?;
    let before: Vec<Duration> = supervisor.history().collect();
    let decision = supervisor
        .stopped(0, Policy::Permanent, Exit::Abnormal, now)
        .context("journald restart supervisor has no child 0")?;
    Ok(match decision {
        Decision::Restart(_) => {
            let delay = if supervisor.recent_restarts(now) > 1 {
                supervisor.restart_delay(BASE_DELAY, MAX_DELAY, now)
            } else {
                Duration::ZERO
            };
            RestartDecision {
                verdict: Verdict::Spawn { delay },
                history: supervisor.history().collect(),
            }
        }
        Decision::Escalate | Decision::AlreadyEscalated | Decision::Ignore => RestartDecision {
            verdict: Verdict::Refuse {
                recent: supervisor.recent_restarts(now).saturating_sub(1),
            },
            history: before,
        },
    })
}

pub(super) fn now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
}

pub(super) fn read_history(path: &Path) -> Result<Vec<Duration>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("read restart history {}", path.display()));
        }
    };
    Ok(text
        .lines()
        .filter_map(|line| line.trim().parse::<u64>().ok())
        .map(Duration::from_nanos)
        .collect())
}

pub(super) fn write_history(path: &Path, history: &[Duration]) -> Result<()> {
    let mut text = String::new();
    for at in history {
        text.push_str(&at.as_nanos().to_string());
        text.push('\n');
    }
    std::fs::write(path, text).with_context(|| format!("write restart history {}", path.display()))
}

pub(super) fn refusal_error(socket_path: &Path, recent: u32) -> anyhow::Error {
    anyhow::anyhow!(
        "journald for {} restarted {recent} times in the last {}s and keeps dying; \
         refusing to respawn until the window passes",
        socket_path.display(),
        WINDOW.as_secs(),
    )
}

#[cfg(test)]
mod tests;
