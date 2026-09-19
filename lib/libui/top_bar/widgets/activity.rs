//! A small breathing eye plus independently admitted, steady-text status widgets.

use std::time::{Duration, Instant};

use tokio::sync::watch;

use super::super::{BarWidget, Content, Side, Tone, Update};
use crate::activity::{Activity, Phase, Snapshot};

const ANIMATION_TICK: Duration = Duration::from_millis(100);

pub(in crate::top_bar) fn new(source: watch::Receiver<Snapshot>) -> Vec<BarWidget> {
    ["activity-eye", "activity-status", "activity-agents"]
        .into_iter()
        .enumerate()
        .map(|(part, id)| {
            let mut source = source.clone();
            let started = tokio::time::Instant::now();
            BarWidget::text(id, Side::Left, 255 - part as u8, Content::default()).with_refresh(
                move |cached, _| {
                    let snapshot = source.borrow_and_update();
                    let now = tokio::time::Instant::now();
                    let (content, next) = present(&snapshot, now.into_std(), now - started, part);
                    let changed = *cached != content;
                    *cached = content;
                    Update { changed, next }
                },
            )
        })
        .collect()
}

fn tone(activity: &Activity, now: Instant) -> Tone {
    match activity.phase {
        Phase::Failed | Phase::Disconnected => Tone::Error,
        Phase::NeedsInput | Phase::Reconnecting => Tone::Warning,
        _ if activity.is_quiet(now) => Tone::Warning,
        Phase::Starting | Phase::Working | Phase::Tools => Tone::Accent,
        _ => Tone::Dim,
    }
}

fn present(
    snapshot: &Snapshot,
    now: Instant,
    elapsed: Duration,
    part: usize,
) -> (Content, Option<Duration>) {
    let selected_tone = tone(&snapshot.selected, now);
    let mut eye_tone = selected_tone;
    let mut active = 0;
    let mut quiet = 0;
    let mut waiting = 0;
    let mut attention = 0;
    let mut errors = 0;
    let mut has_active = snapshot.selected.is_active();
    for (_, activity) in &snapshot.others {
        has_active |= activity.is_active();
        match tone(activity, now) {
            Tone::Error => errors += 1,
            Tone::Warning if activity.is_quiet(now) => quiet += 1,
            Tone::Warning => attention += 1,
            _ if activity.is_active() => active += 1,
            _ if activity.phase == Phase::WaitingAgents => waiting += 1,
            _ => {}
        }
    }
    if errors > 0 {
        eye_tone = Tone::Error;
    } else if quiet + attention > 0 && eye_tone != Tone::Error {
        eye_tone = Tone::Warning;
    } else if active > 0 && eye_tone == Tone::Dim {
        eye_tone = Tone::Accent;
    }
    let breathing =
        has_active && snapshot.animations && !matches!(eye_tone, Tone::Warning | Tone::Error);
    // Quiet clocks still update with animations disabled; idle has no timer.
    let next = if part == 0 && breathing {
        Some(ANIMATION_TICK)
    } else if has_active {
        Some(Duration::from_secs(1))
    } else {
        None
    };
    let content = match part {
        0 => {
            let intensity = breathing.then(|| {
                let phase = elapsed.as_secs_f32() % 2.8 / 2.8;
                ((1.0 - (phase * std::f32::consts::TAU).cos()) * 50.0) as u8
            });
            Content::new("◉").tone(eye_tone).intensity(intensity)
        }
        1 => Content::new(snapshot.selected.label(now)).tone(selected_tone),
        _ => {
            let counts = [
                (active, "active"),
                (quiet, "quiet"),
                (waiting, "waiting"),
                (attention, "need attention"),
                (errors, "failed/disconnected"),
            ];
            let labels: Vec<_> = counts
                .into_iter()
                .filter(|(count, _)| *count > 0)
                .map(|(count, label)| format!("{count} {label}"))
                .collect();
            let text = if labels.is_empty() {
                String::new()
            } else {
                format!("Others: {}", labels.join(" · "))
            };
            Content::new(text).tone(if errors > 0 {
                Tone::Error
            } else if quiet + attention > 0 {
                Tone::Warning
            } else {
                Tone::Normal
            })
        }
    };
    (content, next)
}

#[cfg(test)]
mod tests;
