//! The instrument band: the dials an operator reads first.

use serde::Serialize;

use super::super::board::Board;
use super::super::view::Widget;
use super::{compact, short};

/// Arc length of the dial's track, as a fraction of the full circle.
const DIAL_SWEEP: f64 = 0.75;
const DIAL_R: f64 = 34.0;

#[derive(Serialize)]
pub(in crate::monitor) struct Instruments {
    pub context: Dial,
    pub cache: Dial,
    pub tokens_in: String,
    pub tokens_out: String,
    pub in_share: u8,
    pub limits: Vec<Gauge>,
}

#[derive(Serialize)]
pub(in crate::monitor) struct Dial {
    pub label: String,
    pub value: Option<u8>,
    pub level: &'static str,
    pub track: String,
    pub fill: String,
    pub note: Option<String>,
}

#[derive(Serialize)]
pub(in crate::monitor) struct Gauge {
    pub label: String,
    pub used: u8,
    /// How much of the window has elapsed: using more than this runs out early.
    pub pace: Option<u8>,
    pub ahead: bool,
}

impl Widget for Instruments {
    const NAME: &'static str = "instruments";
}

fn dial(label: &str, value: Option<u8>, level: &'static str, note: Option<String>) -> Dial {
    let circumference = std::f64::consts::TAU * DIAL_R;
    let track = circumference * DIAL_SWEEP;
    let filled = track * f64::from(value.unwrap_or(0)) / 100.0;
    Dial {
        label: label.into(),
        value,
        level,
        track: format!("{track:.1} {circumference:.1}"),
        fill: format!("{filled:.1} {circumference:.1}"),
        note,
    }
}

fn context_level(percent: u8) -> &'static str {
    match percent {
        80.. => "high",
        60.. => "mid",
        _ => "low",
    }
}

impl Instruments {
    pub(in crate::monitor) fn from(board: &Board, now_ms: i64) -> Self {
        let fullest = board
            .sessions
            .iter()
            .filter_map(|(id, session)| {
                let (used, window) = session.context?;
                let window = window.filter(|window| *window > 0)?;
                Some((((used.max(0) * 100) / window).min(100) as u8, id))
            })
            .max_by_key(|(percent, _)| *percent);
        let context = match fullest {
            Some((percent, id)) => dial(
                "context",
                Some(percent),
                context_level(percent),
                Some(format!("fullest: …{}", short(&id.to_string()))),
            ),
            None => dial("context", None, "low", None),
        };
        let (input, cached, output) = board.sessions.values().fold((0, 0, 0), |acc, s| {
            (
                acc.0 + s.usage.input.max(0),
                acc.1 + s.usage.cache_read.max(0),
                acc.2 + s.usage.output.max(0),
            )
        });
        let hit = (input > 0).then(|| ((cached * 100) / input).min(100) as u8);
        let cache = dial(
            "cache hit",
            hit,
            match hit {
                Some(hit) if hit < 40 && input >= 30_000 => "high",
                _ => "low",
            },
            (input > 0).then(|| format!("{} of {}", compact(cached), compact(input))),
        );
        let now_s = now_ms / 1_000;
        let limits = board
            .limits
            .iter()
            .map(|window| {
                let pace =
                    window
                        .window_minutes
                        .zip(window.resets_at)
                        .and_then(|(minutes, resets_at)| {
                            let length = minutes * 60;
                            (length > 0).then(|| {
                                let left = (resets_at - now_s).clamp(0, length);
                                (100 - left * 100 / length) as u8
                            })
                        });
                Gauge {
                    label: window
                        .window_minutes
                        .map_or_else(|| "window".into(), window_label),
                    used: window.used_percent,
                    pace,
                    ahead: pace.is_some_and(|pace| window.used_percent > pace.saturating_add(5)),
                }
            })
            .collect();
        let total = input + output;
        Self {
            context,
            cache,
            tokens_in: compact(input),
            tokens_out: compact(output),
            in_share: if total > 0 {
                ((input * 100) / total) as u8
            } else {
                50
            },
            limits,
        }
    }
}

fn window_label(minutes: i64) -> String {
    match minutes {
        10_080 => "week".into(),
        1_440 => "day".into(),
        m if m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{m}m"),
    }
}
