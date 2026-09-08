//! Platform mechanism, colored by the session policy (not per-command escalation).

use chaos_ipc::protocol::SandboxPolicy;
use chaos_sysinfo::SandboxKind;

use super::super::{BarWidget, Content, Side, Tone};

pub(in crate::top_bar) fn tone(policy: Option<&SandboxPolicy>) -> Tone {
    match policy {
        Some(SandboxPolicy::ReadOnly { .. } | SandboxPolicy::WorkspaceWrite { .. }) => {
            Tone::Success
        }
        Some(SandboxPolicy::RootAccess) => Tone::Error,
        Some(SandboxPolicy::ExternalSandbox { .. }) | None => Tone::Normal,
    }
}

pub(in crate::top_bar) fn new(kind: SandboxKind) -> BarWidget {
    let label = match kind {
        SandboxKind::None => "",
        SandboxKind::Seatbelt => "seatbelt",
        SandboxKind::Seccomp => "seccomp",
        SandboxKind::Capsicum => "capsicum",
    };
    BarWidget::text("sandbox", Side::Left, 160, Content::new(label))
}
