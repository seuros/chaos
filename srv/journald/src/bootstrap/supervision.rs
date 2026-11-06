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
    pub history: Vec<u64>,
}

fn intensity() -> Intensity {
    Intensity::new(MAX_RESTARTS, duration_ns(WINDOW))
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

pub(super) fn decide(history: impl IntoIterator<Item = u64>, now_ns: u64) -> RestartDecision {
    let mut supervisor =
        RestartSupervisor::restore(Strategy::OneForOne, intensity(), history, now_ns);
    let before: Vec<u64> = supervisor.history().collect();
    match supervisor.stopped(0, Policy::Permanent, Exit::Abnormal, now_ns) {
        Decision::Restart(_) => {
            let delay = if supervisor.recent_restarts(now_ns) > 1 {
                Duration::from_nanos(supervisor.restart_delay_ns(
                    duration_ns(BASE_DELAY),
                    duration_ns(MAX_DELAY),
                    now_ns,
                ))
            } else {
                Duration::ZERO
            };
            RestartDecision {
                verdict: Verdict::Spawn { delay },
                history: supervisor.history().collect(),
            }
        }
        Decision::Escalate | Decision::Ignore => RestartDecision {
            verdict: Verdict::Refuse {
                recent: supervisor.recent_restarts(now_ns).saturating_sub(1),
            },
            history: before,
        },
    }
}

pub(super) fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(duration_ns)
        .unwrap_or(0)
}

pub(super) fn read_history(path: &Path) -> Result<Vec<u64>> {
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
        .collect())
}

pub(super) fn write_history(path: &Path, history: &[u64]) -> Result<()> {
    let mut text = String::new();
    for at_ns in history {
        text.push_str(&at_ns.to_string());
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
