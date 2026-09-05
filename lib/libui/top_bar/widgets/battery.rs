//! Battery presentation consumes snapshots published by the shared runtime.

use chaos_sysinfo::PowerInfo;
use tokio::sync::watch;

use super::super::{BarWidget, Content, Side, Tone};

pub(in crate::top_bar) fn new(source: watch::Receiver<PowerInfo>) -> BarWidget {
    BarWidget::watched("battery", Side::Right, 240, source, |power| *power, present)
}

fn present(power: PowerInfo) -> Content {
    if !power.has_battery {
        return Content::default();
    }
    let level = power
        .battery_level
        .map(|level| level.to_string())
        .unwrap_or_else(|| "?".into());
    let (icon, tone) = if power.charger_connected {
        ("⚡", Tone::Success)
    } else {
        (
            "●",
            match power.battery_level {
                Some(0..=15) => Tone::Error,
                Some(16..=30) | None => Tone::Warning,
                Some(_) => Tone::Normal,
            },
        )
    };
    Content::new(format!("{icon} {level}%")).tone(tone)
}
