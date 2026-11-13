//! The station map: the resident at the centre, its stations on a ring, and
//! what is moving between them. Drawn on the server as one SVG; animations
//! are phased from the server clock, so replacing the SVG never restarts them.

use std::hash::{Hash, Hasher};

use chaos_ipc::ProcessId;
use chaos_kern::activity::{Station, TurnOutcome};
use serde::Serialize;

use super::super::board::{Board, STATIONS};
use super::super::view::{Widget, name};

const WIDTH: f64 = 760.0;
const HEIGHT: f64 = 470.0;
const CX: f64 = WIDTH / 2.0;
/// Above centre: labels hang below each node, so the bottom needs the room.
const CY: f64 = 222.0;
const RX: f64 = 300.0;
const RY: f64 = 165.0;
const NODE_R: f64 = 30.0;
const CORE_R: f64 = 54.0;
const PACKET_S: f64 = 1.6;
const GLITCH_S: f64 = 4.0;
const SHOCK_S: f64 = 2.4;
const BREATH_MS: i64 = 2_400;
/// Session colours; a session keeps its colour for as long as it lives.
const HUES: usize = 8;

#[derive(Serialize)]
pub(in crate::monitor) struct Map {
    pub width: f64,
    pub height: f64,
    /// Contour rings around the core: (rx, ry).
    pub rings: Vec<(f64, f64)>,
    /// Phase of the shared breathing loop, so every redraw stays in step.
    pub breath: String,
    pub core: Core,
    pub nodes: Vec<Node>,
    pub packets: Vec<Packet>,
    pub legs: Vec<Leg>,
}

#[derive(Serialize)]
pub(in crate::monitor) struct Core {
    pub x: f64,
    pub y: f64,
    pub r: f64,
    pub working: usize,
    pub sessions: usize,
    /// Seconds into the turn-end shockwave, while it is still playing.
    pub shock: Option<Phase>,
}

#[derive(Serialize)]
pub(in crate::monitor) struct Phase {
    pub delay: String,
    pub tone: &'static str,
}

#[derive(Serialize)]
pub(in crate::monitor) struct Node {
    pub station: String,
    pub x: f64,
    pub y: f64,
    pub r: f64,
    pub label_y: f64,
    pub spoke: String,
    pub active: usize,
    pub done: u64,
    pub failed: u64,
    pub glitch: Option<Phase>,
    pub pips: Vec<Pip>,
}

#[derive(Serialize)]
pub(in crate::monitor) struct Pip {
    pub x: f64,
    pub y: f64,
    pub hue: usize,
}

#[derive(Serialize)]
pub(in crate::monitor) struct Packet {
    pub station: String,
    pub path: String,
    pub hue: usize,
    pub dur: f64,
    pub begin: String,
}

#[derive(Serialize)]
pub(in crate::monitor) struct Leg {
    pub path: String,
    pub opacity: f64,
    pub newest: bool,
}

impl Widget for Map {
    const NAME: &'static str = "map";
}

pub(in crate::monitor) fn hue(id: &ProcessId) -> usize {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.to_string().hash(&mut hasher);
    (hasher.finish() % HUES as u64) as usize
}

fn position(index: usize) -> (f64, f64) {
    // Start at the top and go clockwise.
    let angle =
        -std::f64::consts::FRAC_PI_2 + index as f64 * std::f64::consts::TAU / STATIONS.len() as f64;
    (CX + RX * angle.cos(), CY + RY * angle.sin())
}

/// A gentle curve from the core's rim to the station's rim.
fn spoke(x: f64, y: f64) -> String {
    let (dx, dy) = (x - CX, y - CY);
    let length = dx.hypot(dy);
    let (ux, uy) = (dx / length, dy / length);
    let (x0, y0) = (CX + ux * CORE_R, CY + uy * CORE_R);
    let (x1, y1) = (x - ux * NODE_R, y - uy * NODE_R);
    let (mx, my) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (qx, qy) = (mx - uy * 18.0, my + ux * 18.0);
    format!("M{x0:.1} {y0:.1} Q{qx:.1} {qy:.1} {x1:.1} {y1:.1}")
}

/// Station to station, bent toward the core so legs never cross a node.
fn leg((x0, y0): (f64, f64), (x1, y1): (f64, f64)) -> String {
    let (mx, my) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (qx, qy) = (mx + (CX - mx) * 0.45, my + (CY - my) * 0.45);
    format!("M{x0:.1} {y0:.1} Q{qx:.1} {qy:.1} {x1:.1} {y1:.1}")
}

/// How far into a one-shot animation of `length` seconds we are, if it is
/// still playing; as a negative delay so a fresh SVG picks up mid-way.
fn one_shot(now_ms: i64, at_ms: i64, length: f64) -> Option<String> {
    let age = (now_ms - at_ms).max(0) as f64 / 1_000.0;
    (age < length).then(|| format!("-{age:.2}s"))
}

impl Map {
    pub(in crate::monitor) fn from(board: &Board, now_ms: i64) -> Self {
        let mut nodes = Vec::with_capacity(STATIONS.len());
        let mut packets = Vec::new();
        for (index, station) in STATIONS.iter().enumerate() {
            let (x, y) = position(index);
            let path = spoke(x, y);
            let tally = board.stations.get(station).copied().unwrap_or_default();
            let mut pips = Vec::new();
            let mut active = 0;
            for (id, session) in &board.sessions {
                let calls: Vec<_> = session
                    .calls
                    .values()
                    .filter(|call| call.station == *station)
                    .collect();
                if calls.is_empty() {
                    continue;
                }
                active += calls.len();
                let hue = hue(id);
                let angle = -2.2 + pips.len() as f64 * 0.42;
                pips.push(Pip {
                    x: x + (NODE_R + 7.0) * angle.cos(),
                    y: y + (NODE_R + 7.0) * angle.sin(),
                    hue,
                });
                for call in calls {
                    let elapsed = (now_ms - call.started_ms).max(0) as f64 / 1_000.0;
                    packets.push(Packet {
                        station: name(station),
                        path: path.clone(),
                        hue,
                        dur: PACKET_S,
                        begin: format!("-{:.2}s", elapsed % PACKET_S),
                    });
                }
            }
            nodes.push(Node {
                station: name(station),
                x,
                y,
                r: NODE_R,
                label_y: y + NODE_R + 16.0,
                spoke: path,
                active,
                done: tally.done,
                failed: tally.failed,
                glitch: tally
                    .failed_at_ms
                    .and_then(|at| one_shot(now_ms, at, GLITCH_S))
                    .map(|delay| Phase { delay, tone: "bad" }),
                pips,
            });
        }
        let at = |station: &Station| {
            STATIONS
                .iter()
                .position(|candidate| candidate == station)
                .map(position)
        };
        let stops: Vec<_> = board.route.iter().filter_map(|(s, _)| at(s)).collect();
        let count = stops.len().saturating_sub(1);
        let legs = stops
            .windows(2)
            .enumerate()
            .map(|(index, pair)| Leg {
                path: leg(pair[0], pair[1]),
                opacity: 0.12 + 0.5 * (index + 1) as f64 / count.max(1) as f64,
                newest: index + 1 == count,
            })
            .collect();
        let working = board.sessions.values().filter(|s| s.working).count();
        Self {
            width: WIDTH,
            height: HEIGHT,
            rings: [0.38, 0.69, 1.0]
                .iter()
                .map(|scale| (RX * scale, RY * scale))
                .collect(),
            breath: format!("-{:.2}s", (now_ms.rem_euclid(BREATH_MS)) as f64 / 1_000.0),
            core: Core {
                x: CX,
                y: CY,
                r: CORE_R,
                working,
                sessions: board.sessions.len(),
                shock: board.turn_ended.and_then(|(at, outcome)| {
                    one_shot(now_ms, at, SHOCK_S).map(|delay| Phase {
                        delay,
                        tone: match outcome {
                            TurnOutcome::Completed => "good",
                            TurnOutcome::Aborted => "bad",
                        },
                    })
                }),
            },
            nodes,
            packets,
            legs,
        }
    }
}
