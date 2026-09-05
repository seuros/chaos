//! Available platform sandbox mechanism, not the current permission policy.

use chaos_sysinfo::SandboxKind;

use super::super::{BarWidget, Content, Side};

pub(in crate::top_bar) fn new(kind: SandboxKind) -> BarWidget {
    let label = match kind {
        SandboxKind::None => "",
        SandboxKind::Seatbelt => "seatbelt",
        SandboxKind::Seccomp => "seccomp",
        SandboxKind::Capsicum => "capsicum",
    };
    BarWidget::text("sandbox", Side::Left, 160, Content::new(label))
}
