use chaos_kern::machine_status::{MachineStatus, MachineWarning};

use super::super::machine::Source;
use super::super::{BarWidget, Content, Tone};

pub(in crate::top_bar) fn new(source: Source) -> BarWidget {
    super::observed("disk", 210, source, present)
}

fn present(status: &MachineStatus) -> Content {
    let storage = &status.storage;
    let mut partial = !storage.unavailable.is_empty();
    let mut relevant = false;
    let minimum = storage
        .filesystems
        .iter()
        .filter(|filesystem| !filesystem.targets.is_empty())
        .filter_map(|filesystem| {
            relevant = true;
            let percent = filesystem.available_percent();
            partial |= percent.is_none();
            percent
        })
        .min_by(f64::total_cmp);
    let text = match minimum {
        Some(percent) => format!("disk {percent:.1}% free{}", if partial { " ?" } else { "" }),
        None if partial || relevant => "disk ?".into(),
        None => return Content::default(),
    };
    let tone = if status
        .warnings
        .iter()
        .any(|warning| matches!(warning, MachineWarning::LowDiskSpace { .. }))
    {
        Tone::Error
    } else {
        Tone::Normal
    };
    Content::new(text).tone(tone)
}
