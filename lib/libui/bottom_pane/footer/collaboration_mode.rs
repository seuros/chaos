use ratatui::text::Line;
use ratatui::text::Span;

use super::types::CollaborationModeIndicator;
use super::types::ModeBadgeDetail;

const MODE_CYCLE_HINT: &str = "shift+tab to cycle";

impl CollaborationModeIndicator {
    fn suffix(show_cycle_hint: bool) -> String {
        if show_cycle_hint {
            format!(" ({MODE_CYCLE_HINT})")
        } else {
            String::new()
        }
    }

    pub(super) fn label(&self, detail: ModeBadgeDetail, show_cycle_hint: bool) -> String {
        let mode = self.kind.display_name();
        let base = match detail {
            ModeBadgeDetail::Full => mode.to_string(),
            ModeBadgeDetail::Compact => mode.to_string(),
            ModeBadgeDetail::ModeOnly => mode.to_string(),
        };
        format!("{base}{}", Self::suffix(show_cycle_hint))
    }

    pub(super) fn styled_span(
        &self,
        detail: ModeBadgeDetail,
        show_cycle_hint: bool,
    ) -> Span<'static> {
        let label = self.label(detail, show_cycle_hint);
        Span::styled(label, crate::theme::collaboration_mode_badge(self.kind))
    }
}

pub(super) fn mode_indicator_line(
    indicator: Option<CollaborationModeIndicator>,
    detail: ModeBadgeDetail,
    show_cycle_hint: bool,
) -> Option<Line<'static>> {
    indicator.map(|indicator| Line::from(vec![indicator.styled_span(detail, show_cycle_hint)]))
}
