use super::domain::FOOTER_SEPARATOR;
pub(super) use crate::bottom_pane::footer_tips::FooterTip;

pub(super) fn wrap_footer_tips(width: u16, tips: Vec<FooterTip>) -> Vec<Vec<FooterTip>> {
    crate::bottom_pane::footer_tips::wrap_footer_tips(width, FOOTER_SEPARATOR, tips)
}
