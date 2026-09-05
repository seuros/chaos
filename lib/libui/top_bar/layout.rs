//! Priority admission followed by bounded left/right layout.

use std::cmp::Reverse;

use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Margin;
use ratatui::layout::Rect;

use super::Side;
use super::WidgetSpec;

const SEPARATOR_WIDTH: usize = 3;
const GROUP_GAP: usize = 1;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Placement {
    pub(super) index: usize,
    pub(super) area: Rect,
    pub(super) separator: Option<Rect>,
}

pub(super) fn arrange(area: Rect, specs: &[WidgetSpec]) -> Vec<Placement> {
    if area.is_empty() {
        return Vec::new();
    }
    let inner = area.inner(Margin::new(1, 0));
    let mut candidates: Vec<_> = (0..specs.len()).collect();
    // Stable sort: equal priorities retain declaration order.
    candidates.sort_by_key(|&index| Reverse(specs[index].priority));
    let mut visible = vec![false; specs.len()];
    let mut widths = [0usize; 2];
    let mut counts = [0usize; 2];

    for index in candidates {
        let spec = specs[index];
        let side = usize::from(spec.side == Side::Right);
        let width = spec.min_width;
        let separator = if counts[side] > 0 { SEPARATOR_WIDTH } else { 0 };
        let group_gap = if counts[1 - side] > 0 { GROUP_GAP } else { 0 };
        let needed = widths[0] + widths[1] + separator + group_gap;
        // Compare before adding the arbitrary content width; no narrowing cast
        // or overflow can make a too-wide widget look small enough to admit.
        if width == 0 || width > usize::from(inner.width).saturating_sub(needed) {
            continue;
        }
        visible[index] = true;
        widths[side] += width + separator;
        counts[side] += 1;
    }

    let [left, _, right] = Layout::horizontal([
        Constraint::Length(widths[0] as u16),
        Constraint::Fill(1),
        Constraint::Length(widths[1] as u16),
    ])
    .areas(inner);

    let mut placements = Vec::new();
    for (side, group) in [(Side::Left, left), (Side::Right, right)] {
        let mut x = group.x;
        let mut first = true;
        for (index, spec) in specs.iter().enumerate() {
            if !visible[index] || spec.side != side {
                continue;
            }
            let separator = if first {
                None
            } else {
                let area = Rect::new(x, group.y, SEPARATOR_WIDTH as u16, 1);
                x += area.width;
                Some(area)
            };
            let area = Rect::new(x, group.y, spec.min_width as u16, 1);
            placements.push(Placement {
                index,
                area,
                separator,
            });
            x += area.width;
            first = false;
        }
    }
    placements
}
