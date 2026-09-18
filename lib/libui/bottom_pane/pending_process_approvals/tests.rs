use super::*;
use crate::test_support::renderable_first_char_string_with_size;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;

pub(crate) fn pending_process_approvals_suite() {
    desired_height_empty();
    render_single_process_snapshot();
    render_multiple_processes_snapshot();
}

fn desired_height_empty() {
    let widget = PendingProcessApprovals::new();
    assert_eq!(widget.desired_height(40), 0);
}

fn render_single_process_snapshot() {
    let mut widget = PendingProcessApprovals::new();
    widget.set_processes(vec!["Robie [scout]".to_string()]);
    let width = 40;

    assert_snapshot!(
        renderable_first_char_string_with_size(&widget, width, widget.desired_height(width))
            .replace(' ', "."),
        @r"
..!.Approval.needed.in.Robie.[scout]....
..../agent.to.switch.processes.........."
    );
}

fn render_multiple_processes_snapshot() {
    let mut widget = PendingProcessApprovals::new();
    widget.set_processes(vec![
        "Main [default]".to_string(),
        "Robie [scout]".to_string(),
        "Inspector".to_string(),
        "Extra agent".to_string(),
    ]);
    let width = 44;

    assert_snapshot!(
        renderable_first_char_string_with_size(&widget, width, widget.desired_height(width))
            .replace(' ', "."),
        @r"
..!.Approval.needed.in.Main.[default].......
..!.Approval.needed.in.Robie.[scout]........
..!.Approval.needed.in.Inspector............
............................................
..../agent.to.switch.processes.............."
    );
}
