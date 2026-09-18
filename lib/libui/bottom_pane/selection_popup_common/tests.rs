use super::*;
use pretty_assertions::assert_eq;

pub(crate) fn one_cell_width_falls_back_without_panic_for_wrapped_two_column_rows() {
    let row = GenericDisplayRow {
        name: "1. Very long option label".to_string(),
        description: Some("Very long description".to_string()),
        wrap_indent: Some(4),
        ..Default::default()
    };

    let two_col = wrap_two_column_row(&row, 0, 1);
    assert_eq!(two_col.len(), 0);
}
