use chaos_machine::{DisplayState, ExecutionEnvironment, FormFactor};

use super::super::machine::Source;
use super::super::{BarWidget, Content, Side};

pub(in crate::top_bar) fn new(source: Source) -> BarWidget {
    BarWidget::watched("profile", Side::Left, 140, source, |snapshot| {
        let Some(status) = snapshot else {
            return Content::new("machine ?");
        };
        let profile = &status.machine.profile;
        let mut labels = Vec::new();
        if let Some(label) = match profile.form_factor {
            FormFactor::Laptop => Some("laptop"),
            FormFactor::Desktop => Some("desktop"),
            FormFactor::Server => Some("server"),
            FormFactor::Tablet => Some("tablet"),
            FormFactor::Other => Some("other"),
            FormFactor::Unknown => None,
        } {
            labels.push(label);
        }
        if profile.displays == DisplayState::NoneDetected {
            labels.push("headless");
        }
        if profile.execution_environment == ExecutionEnvironment::VirtualMachine {
            labels.push("VM");
        }
        if profile.session.ssh {
            labels.push("SSH");
        }
        Content::new(labels.join("/"))
    })
}
