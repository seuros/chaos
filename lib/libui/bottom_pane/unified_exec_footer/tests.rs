use super::*;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;

pub(crate) fn unified_exec_footer_suite() {
    desired_height_empty();
    render_more_sessions();
    render_many_sessions();
}

fn desired_height_empty() {
    let footer = UnifiedExecFooter::new();
    assert_eq!(footer.desired_height(40), 0);
}

fn render_more_sessions() {
    let mut footer = UnifiedExecFooter::new();
    footer.set_processes(vec!["rg \"foo\" src".to_string()]);
    let width = 50;
    let height = footer.desired_height(width);
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
    footer.render(Rect::new(0, 0, width, height), &mut buf);
    assert_snapshot!("render_more_sessions", format!("{buf:?}"));
}

fn render_many_sessions() {
    let mut footer = UnifiedExecFooter::new();
    footer.set_processes((0..123).map(|idx| format!("cmd {idx}")).collect());
    let width = 50;
    let height = footer.desired_height(width);
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
    footer.render(Rect::new(0, 0, width, height), &mut buf);
    assert_snapshot!("render_many_sessions", format!("{buf:?}"));
}
