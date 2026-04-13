use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;

use super::types::CollaborationModeIndicator;

const MODE_CYCLE_HINT: &str = "shift+tab to cycle";

impl CollaborationModeIndicator {
    pub(super) fn label(self, show_cycle_hint: bool) -> String {
        let suffix = if show_cycle_hint {
            format!(" ({MODE_CYCLE_HINT})")
        } else {
            String::new()
        };
        match self {
            CollaborationModeIndicator::Plan => format!("Plan mode{suffix}"),
            CollaborationModeIndicator::PairProgramming => {
                format!("Pair Programming mode{suffix}")
            }
            CollaborationModeIndicator::Execute => format!("Execute mode{suffix}"),
        }
    }

    pub(super) fn styled_span(self, show_cycle_hint: bool) -> Span<'static> {
        let label = self.label(show_cycle_hint);
        match self {
            CollaborationModeIndicator::Plan => Span::from(label).magenta(),
            CollaborationModeIndicator::PairProgramming => Span::from(label).cyan(),
            CollaborationModeIndicator::Execute => Span::from(label).dim(),
        }
    }
}

pub fn mode_indicator_line(
    indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
) -> Option<Line<'static>> {
    indicator.map(|indicator| Line::from(vec![indicator.styled_span(show_cycle_hint)]))
}
